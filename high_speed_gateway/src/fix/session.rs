//! FIX session layer as a pure state machine: no sockets, no clock.
//!
//! The connection feeds it parsed frames (`on_frame`) and timer ticks
//! (`on_timer`), with the current time in nanoseconds since the Unix epoch.
//! Outbound messages go to an `Outbox`; the returned `Status` says whether to
//! drop the connection.
//!
//! Covers logon (acceptor and initiator), heartbeats and TestRequest,
//! sequence numbers (gaps -> ResendRequest, too low -> Logout, PossDup),
//! ResendRequest (answered with a gap fill: there's no message store yet, so
//! application messages aren't resent either), SequenceReset, Reject and Logout.
//!
//! Application messages that pass the session checks go to an `Application`
//! (see app.rs), in sequence order. Its refusals, and business rules the
//! schema can't express, are answered with a BusinessMessageReject (35=j), as
//! are MsgTypes FIX defines but the dictionary doesn't (380=3); MsgTypes FIX
//! doesn't define get a Reject (373=11).
//! It sends through a `Sender`, which only takes application messages and
//! only while the session is active.

use super::FixVersion;
use super::app::{self, AppReject, Application, BusinessReject, Inbound};
use super::decode;
use super::encoder::{EncodeError, MessageWriter};
use super::message::Message;
use super::parser::{ParseOutcome, RejectReason};
use super::spec::{self, BusinessRejectReason, MsgType, SessionRejectReason, tags};
use super::validate::{Header, HeaderError, HeaderRules, SessionReject, validate_header};
use super::views::{self, SessionMessage};

const SECOND: i64 = 1_000_000_000;
/// Largest outbound message.
const OUT_BUF: usize = 4096;

/// Where outbound messages go, e.g. the socket's send buffer.
pub trait Outbox {
    fn send(&mut self, message: &[u8]);
}

impl Outbox for Vec<Vec<u8>> {
    fn send(&mut self, message: &[u8]) {
        self.push(message.to_vec());
    }
}

#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Continue,
    /// Close the connection. Anything already in the outbox should still be flushed.
    Disconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Waits for the counterparty's Logon.
    Acceptor,
    /// Sends the first Logon.
    Initiator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Initiator before `start`.
    Disconnected,
    /// Acceptor waiting for the counterparty's Logon.
    AwaitingLogon,
    /// Initiator waiting for the Logon reply.
    LogonSent,
    Active,
    /// We sent Logout and wait for theirs.
    LogoutSent,
    Closed,
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub role: Role,
    pub version: FixVersion,
    pub our_comp_id: String,
    pub their_comp_id: String,
    /// HeartBtInt we ask for as initiator. An acceptor uses the counterparty's.
    pub heart_bt_int_secs: u32,
    /// Initiator: send ResetSeqNumFlag=Y on Logon.
    pub reset_on_logon: bool,
    pub max_sending_time_skew_ns: i64,
    /// How long to wait for a Logon (or the reply to ours).
    pub logon_timeout_ns: i64,
    /// How long to wait for the Logout reply.
    pub logout_timeout_ns: i64,
}

impl SessionConfig {
    pub fn new(role: Role, our_comp_id: &str, their_comp_id: &str) -> Self {
        Self {
            role,
            version: FixVersion::Fix44,
            our_comp_id: our_comp_id.to_string(),
            their_comp_id: their_comp_id.to_string(),
            heart_bt_int_secs: 30,
            reset_on_logon: false,
            max_sending_time_skew_ns: 120 * SECOND,
            logon_timeout_ns: 10 * SECOND,
            logout_timeout_ns: 10 * SECOND,
        }
    }
}

pub struct Session {
    config: SessionConfig,
    state: SessionState,

    /// MsgSeqNum we expect next from the counterparty.
    next_in_seq: u32,
    /// MsgSeqNum of our next message.
    next_out_seq: u32,
    /// Highest MsgSeqNum asked for in our pending ResendRequest.
    resend_until: Option<u32>,

    heartbeat_ns: i64,
    last_rx: i64,
    last_tx: i64,
    /// When the current state started (logon/logout timeouts).
    state_since: i64,
    /// Outstanding TestRequest: its id and when it went out.
    test_request: Option<(u64, i64)>,
    test_req_counter: u64,

    out: [u8; OUT_BUF],
}

impl Session {
    pub fn new(config: SessionConfig, now: i64) -> Self {
        let state = match config.role {
            Role::Acceptor => SessionState::AwaitingLogon,
            Role::Initiator => SessionState::Disconnected,
        };
        let heartbeat_ns = config.heart_bt_int_secs as i64 * SECOND;
        Self {
            config,
            state,
            next_in_seq: 1,
            next_out_seq: 1,
            resend_until: None,
            heartbeat_ns,
            last_rx: now,
            last_tx: now,
            state_since: now,
            test_request: None,
            test_req_counter: 0,
            out: [0; OUT_BUF],
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn next_in_seq(&self) -> u32 {
        self.next_in_seq
    }

    pub fn next_out_seq(&self) -> u32 {
        self.next_out_seq
    }

    /// Initiator: sends the Logon.
    pub fn start(&mut self, now: i64, out: &mut impl Outbox) -> Status {
        if self.config.role != Role::Initiator || self.state != SessionState::Disconnected {
            return Status::Continue
        }
        if self.config.reset_on_logon {
            self.next_in_seq = 1;
            self.next_out_seq = 1;
        }
        let (heart_bt_int, reset) = (self.config.heart_bt_int_secs, self.config.reset_on_logon);
        self.send_logon(heart_bt_int, reset, now, out);
        self.set_state(SessionState::LogonSent, now);
        Status::Continue
    }

    /// Starts an orderly logout.
    pub fn logout(&mut self, text: &str, now: i64, out: &mut impl Outbox) -> Status {
        match self.state {
            SessionState::Active => {
                self.send_logout(Some(text), now, out);
                self.set_state(SessionState::LogoutSent, now);
                Status::Continue
            }
            SessionState::LogoutSent => Status::Continue,
            _ => self.close(now),
        }
    }

    /// Timer tick: heartbeats, TestRequest, logon/logout timeouts. Call it at
    /// least once a second.
    pub fn on_timer(&mut self, now: i64, out: &mut impl Outbox) -> Status {
        match self.state {
            SessionState::AwaitingLogon | SessionState::LogonSent => {
                if now - self.state_since >= self.config.logon_timeout_ns {
                    return self.close(now)
                }
            }
            SessionState::LogoutSent => {
                if now - self.state_since >= self.config.logout_timeout_ns {
                    return self.close(now)
                }
            }
            SessionState::Active if self.heartbeat_ns > 0 => {
                if let Some((_, sent)) = self.test_request {
                    if now - sent >= self.heartbeat_ns {
                        self.send_logout(Some("Heartbeat timeout"), now, out);
                        return self.close(now)
                    }
                } else if now - self.last_rx >= self.heartbeat_ns + self.heartbeat_ns / 5 {
                    // A little grace for network delay before testing the line.
                    self.test_req_counter += 1;
                    let id = self.test_req_counter;
                    self.send(MsgType::TestRequest, now, out, |w| w.u64(tags::TEST_REQ_ID, id).map(|_| ()));
                    self.test_request = Some((id, now));
                }
                if now - self.last_tx >= self.heartbeat_ns {
                    self.send(MsgType::Heartbeat, now, out, |_| Ok(()));
                }
            }
            _ => {}
        }
        Status::Continue
    }

    /// One frame from the framer, after parsing. Application messages go to `app`.
    pub fn on_frame(&mut self, outcome: ParseOutcome<'_>, now: i64, out: &mut impl Outbox, app: &mut impl Application) -> Status {
        match outcome {
            // Garbled: drop silently, sequence number unchanged.
            ParseOutcome::Garbled(_) => Status::Continue,
            ParseOutcome::Reject { partial, tag, reason } => {
                self.last_rx = now;
                if !self.logged_on() {
                    return self.close(now)
                }
                let seq = partial.fields.get_slice(tags::MSG_SEQ_NUM, partial.raw).and_then(decode::to_u32);
                if seq == Some(self.next_in_seq) {
                    self.next_in_seq += 1;
                }
                let reject = SessionReject {
                    ref_seq_num: seq,
                    ref_tag: (tag != 0).then_some(tag),
                    ref_msg_type: partial.get_msg_type(),
                    reason: parser_reason(reason),
                    text: "Malformed field",
                };
                self.send_reject(&reject, now, out);
                Status::Continue
            }
            ParseOutcome::Message(msg) => {
                self.last_rx = now;
                self.on_message(&msg, now, out, app)
            }
        }
    }

    fn on_message(&mut self, msg: &Message, now: i64, out: &mut impl Outbox, app: &mut impl Application) -> Status {
        if matches!(self.state, SessionState::Closed | SessionState::Disconnected) {
            return Status::Disconnect
        }
        let rules = HeaderRules {
            our_comp_id: self.config.our_comp_id.as_bytes(),
            their_comp_id: self.config.their_comp_id.as_bytes(),
            max_sending_time_skew_ns: self.config.max_sending_time_skew_ns,
        };

        let header = match validate_header(msg, &rules, now) {
            Ok(header) => header,
            Err(HeaderError::Logout { text }) => {
                self.send_logout(Some(text), now, out);
                return self.close(now)
            }
            Err(HeaderError::RejectAndLogout(r)) => {
                if self.logged_on() {
                    self.send_reject(&r, now, out);
                }
                self.send_logout(Some(r.text), now, out);
                return self.close(now)
            }
            Err(HeaderError::Reject(r)) => {
                if !self.logged_on() {
                    return self.close(now)
                }
                if r.ref_seq_num == Some(self.next_in_seq) {
                    self.next_in_seq += 1;
                }
                self.send_reject(&r, now, out);
                return Status::Continue
            }
        };

        let is_logon = header.msg_type == MsgType::Logon.as_bytes();
        if !self.logged_on() {
            // The first message must be a Logon; anything else is dropped with the line.
            return if is_logon { self.on_logon(msg, &header, now, out, app) } else { self.close(now) }
        }
        if is_logon {
            self.send_logout(Some("Unexpected Logon"), now, out);
            return self.close(now)
        }

        // SequenceReset in reset mode ignores MsgSeqNum.
        let is_reset = header.msg_type == MsgType::SequenceReset.as_bytes();
        if is_reset && msg.get_boolean(tags::GAP_FILL_FLAG) != Some(true) {
            return self.dispatch(msg, &header, now, out, app)
        }

        let seq = header.msg_seq_num;
        if seq > self.next_in_seq {
            // Gap. Logout is still honoured; everything else waits for the resend.
            if header.msg_type == MsgType::Logout.as_bytes() {
                return self.dispatch(msg, &header, now, out, app)
            }
            self.request_resend(seq, now, out);
            return Status::Continue
        }
        if seq < self.next_in_seq {
            if header.poss_dup {
                // Already processed.
                return Status::Continue
            }
            self.send_logout(Some("MsgSeqNum too low"), now, out);
            return self.close(now)
        }

        self.next_in_seq += 1;
        if self.resend_until.is_some_and(|until| self.next_in_seq > until) {
            self.resend_until = None;
        }
        self.dispatch(msg, &header, now, out, app)
    }

    fn dispatch(&mut self, msg: &Message, header: &Header, now: i64, out: &mut impl Outbox, app: &mut impl Application) -> Status {
        match spec::message(header.msg_type) {
            Some(s) if !s.admin => return self.on_app_message(msg, header, now, out, app),
            None if spec::is_defined_msg_type(header.msg_type) => {
                let b = BusinessReject::new(BusinessRejectReason::UnsupportedMessageType, None, "Unsupported message type");
                self.send_business_reject(header, &b, now, out);
                return Status::Continue
            }
            // Ours, or undefined: views::decode rejects the latter with 373=11.
            _ => {}
        }
        let view = match views::decode(msg, header) {
            Ok(view) => view,
            Err(r) => {
                self.send_reject(&r, now, out);
                return Status::Continue
            }
        };

        match view {
            SessionMessage::Heartbeat(hb) => {
                if let (Some((id, _)), Some(echo)) = (self.test_request, hb.test_req_id)
                    && decode::to_u64(echo.as_bytes()) == Some(id)
                {
                    self.test_request = None;
                }
            }
            SessionMessage::TestRequest(tr) => {
                let id = tr.test_req_id;
                self.send(MsgType::Heartbeat, now, out, |w| w.str(tags::TEST_REQ_ID, id).map(|_| ()));
            }
            SessionMessage::ResendRequest(rr) => {
                // Nothing is stored for resending yet, so fill the whole range.
                let new_seq_no = self.next_out_seq;
                if rr.begin_seq_no < new_seq_no {
                    self.send_gap_fill(rr.begin_seq_no, new_seq_no, now, out);
                }
            }
            SessionMessage::Reject(_) => {
                // Our message was rejected. Nothing to undo for admin messages.
            }
            SessionMessage::SequenceReset(reset) => {
                // For a gap fill, next_in_seq already counts this message.
                if reset.new_seq_no < self.next_in_seq {
                    let r = SessionReject {
                        ref_seq_num: Some(header.msg_seq_num),
                        ref_tag: Some(tags::NEW_SEQ_NO),
                        ref_msg_type: Some(header.msg_type),
                        reason: SessionRejectReason::ValueIsIncorrect,
                        text: "NewSeqNo lower than expected MsgSeqNum",
                    };
                    self.send_reject(&r, now, out);
                } else {
                    self.next_in_seq = reset.new_seq_no;
                    if self.resend_until.is_some_and(|until| self.next_in_seq > until) {
                        self.resend_until = None;
                    }
                }
            }
            SessionMessage::Logout(_) => {
                if self.state != SessionState::LogoutSent {
                    self.send_logout(None, now, out);
                }
                return self.close(now)
            }
            SessionMessage::Logon(_) => unreachable!("Logon handled before dispatch"),
        }
        Status::Continue
    }

    fn on_app_message(&mut self, msg: &Message, header: &Header, now: i64, out: &mut impl Outbox, app: &mut impl Application) -> Status {
        let view = match app::decode(msg, header) {
            Ok(view) => view,
            Err(AppReject::Session(r)) => {
                self.send_reject(&r, now, out);
                return Status::Continue
            }
            Err(AppReject::Business(b)) => {
                self.send_business_reject(header, &b, now, out);
                return Status::Continue
            }
        };
        let inbound = Inbound { header, message: msg, view };
        if let Err(b) = app.on_message(&inbound, &mut Sender { session: self, out: &mut *out, now }) {
            self.send_business_reject(header, &b, now, out);
        }
        Status::Continue
    }

    fn on_logon(&mut self, msg: &Message, header: &Header, now: i64, out: &mut impl Outbox, app: &mut impl Application) -> Status {
        // An invalid Logon gets a Logout, not a Reject.
        let logon = match views::decode(msg, header) {
            Ok(SessionMessage::Logon(logon)) => logon,
            Ok(_) => unreachable!("MsgType is Logon"),
            Err(r) => {
                self.send_logout(Some(r.text), now, out);
                return self.close(now)
            }
        };
        if logon.encrypt_method != super::spec::EncryptMethod::NoneOther {
            self.send_logout(Some("Encryption not supported"), now, out);
            return self.close(now)
        }

        if logon.reset_seq_num {
            if header.msg_seq_num != 1 {
                self.send_logout(Some("ResetSeqNumFlag=Y requires MsgSeqNum=1"), now, out);
                return self.close(now)
            }
            self.next_in_seq = 1;
            if self.config.role == Role::Acceptor {
                self.next_out_seq = 1;
            }
        }

        let seq = header.msg_seq_num;
        if seq < self.next_in_seq {
            self.send_logout(Some("MsgSeqNum too low"), now, out);
            return self.close(now)
        }

        self.heartbeat_ns = logon.heart_bt_int as i64 * SECOND;
        if self.config.role == Role::Acceptor {
            self.send_logon(logon.heart_bt_int, logon.reset_seq_num, now, out);
        }
        self.set_state(SessionState::Active, now);

        if seq > self.next_in_seq {
            self.request_resend(seq, now, out);
        } else {
            self.next_in_seq += 1;
        }
        app.on_logon(&mut self.sender(now, out));
        Status::Continue
    }

    /// A `Sender` for application messages outside `Application` callbacks.
    pub fn sender<'a, O: Outbox>(&'a mut self, now: i64, out: &'a mut O) -> Sender<'a, O> {
        Sender { session: self, out, now }
    }

    /// Sends an application message; returns the MsgSeqNum it went out with.
    /// `body` writes everything after the session's header fields.
    pub fn send_app(&mut self, msg_type: MsgType, now: i64, out: &mut impl Outbox,
                    body: impl FnOnce(&mut MessageWriter) -> Result<(), EncodeError>) -> Result<u32, SendError> {
        if self.state != SessionState::Active {
            return Err(SendError::NotActive)
        }
        if !spec::message(msg_type.as_bytes()).is_some_and(|s| !s.admin) {
            return Err(SendError::NotApplicationMessage)
        }
        let seq = self.next_out_seq;
        self.try_send(msg_type, now, out, body).map_err(SendError::Encode)?;
        Ok(seq)
    }

    fn logged_on(&self) -> bool {
        matches!(self.state, SessionState::Active | SessionState::LogoutSent)
    }

    fn set_state(&mut self, state: SessionState, now: i64) {
        self.state = state;
        self.state_since = now;
    }

    fn close(&mut self, now: i64) -> Status {
        self.set_state(SessionState::Closed, now);
        Status::Disconnect
    }

    fn request_resend(&mut self, received: u32, now: i64, out: &mut impl Outbox) {
        // Ours always asks for everything onwards (EndSeqNo=0), so a pending
        // one already covers this message.
        if let Some(until) = self.resend_until {
            self.resend_until = Some(until.max(received));
            return
        }
        let begin = self.next_in_seq as u64;
        self.send(MsgType::ResendRequest, now, out, |w| {
            w.u64(tags::BEGIN_SEQ_NO, begin)?.u64(tags::END_SEQ_NO, 0).map(|_| ())
        });
        self.resend_until = Some(received);
    }

    fn send_logon(&mut self, heart_bt_int: u32, reset: bool, now: i64, out: &mut impl Outbox) {
        self.send(MsgType::Logon, now, out, |w| {
            w.u64(tags::ENCRYPT_METHOD, 0)?.u64(tags::HEART_BT_INT, heart_bt_int as u64)?;
            if reset {
                w.bool(tags::RESET_SEQ_NUM_FLAG, true)?;
            }
            Ok(())
        });
    }

    fn send_logout(&mut self, text: Option<&str>, now: i64, out: &mut impl Outbox) {
        self.send(MsgType::Logout, now, out, |w| {
            if let Some(text) = text {
                w.str(tags::TEXT, text)?;
            }
            Ok(())
        });
    }

    fn send_reject(&mut self, r: &SessionReject, now: i64, out: &mut impl Outbox) {
        self.send(MsgType::Reject, now, out, |w| {
            w.u64(tags::REF_SEQ_NUM, r.ref_seq_num.unwrap_or(0) as u64)?;
            if let Some(tag) = r.ref_tag {
                w.u64(tags::REF_TAG_ID, tag as u64)?;
            }
            if let Some(msg_type) = r.ref_msg_type.filter(|t| !t.is_empty()) {
                w.bytes(tags::REF_MSG_TYPE, msg_type)?;
            }
            w.bytes(tags::SESSION_REJECT_REASON, r.reason.as_bytes())?.str(tags::TEXT, r.text).map(|_| ())
        });
    }

    /// BusinessMessageReject (35=j) for the message `header` belongs to.
    fn send_business_reject(&mut self, header: &Header, b: &BusinessReject, now: i64, out: &mut impl Outbox) {
        let required = |w: &mut MessageWriter| -> Result<(), EncodeError> {
            w.u64(tags::REF_SEQ_NUM, header.msg_seq_num as u64)?
                .bytes(tags::REF_MSG_TYPE, header.msg_type)?
                .bytes(tags::BUSINESS_REJECT_REASON, b.reason.as_bytes())
                .map(|_| ())
        };
        let full = self.try_send(MsgType::BusinessMessageReject, now, out, |w| {
            required(w)?;
            if let Some(id) = b.ref_id.filter(|id| !id.is_empty()) {
                w.str(tags::BUSINESS_REJECT_REF_ID, id)?;
            }
            if !b.text.is_empty() {
                w.str(tags::TEXT, b.text)?;
            }
            Ok(())
        });
        if full.is_err() {
            // The application's ID or text didn't encode (SOH, too long): send the bare reject.
            self.send(MsgType::BusinessMessageReject, now, out, required);
        }
    }

    /// SequenceReset-GapFill for [begin, new_seq_no). It carries MsgSeqNum =
    /// begin and doesn't advance our sequence.
    fn send_gap_fill(&mut self, begin: u32, new_seq_no: u32, now: i64, out: &mut impl Outbox) {
        let bytes = encode(&mut self.out, &self.config, MsgType::SequenceReset, begin, now, |w| {
            w.bool(tags::POSS_DUP_FLAG, true)?
                .timestamp(tags::ORIG_SENDING_TIME, now)?
                .bool(tags::GAP_FILL_FLAG, true)?
                .u64(tags::NEW_SEQ_NO, new_seq_no as u64)
                .map(|_| ())
        });
        out.send(bytes.expect("admin messages fit the output buffer"));
        self.last_tx = now;
    }

    /// Encodes header + `body` with the next outbound MsgSeqNum and sends it.
    fn send(&mut self, msg_type: MsgType, now: i64, out: &mut impl Outbox,
            body: impl FnOnce(&mut MessageWriter) -> Result<(), EncodeError>) {
        self.try_send(msg_type, now, out, body).expect("admin messages fit the output buffer");
    }

    /// Like `send`; on an encoding error nothing is sent and the sequence
    /// number isn't used.
    fn try_send(&mut self, msg_type: MsgType, now: i64, out: &mut impl Outbox,
                body: impl FnOnce(&mut MessageWriter) -> Result<(), EncodeError>) -> Result<(), EncodeError> {
        let bytes = encode(&mut self.out, &self.config, msg_type, self.next_out_seq, now, body)?;
        out.send(bytes);
        self.next_out_seq += 1;
        self.last_tx = now;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SendError {
    /// Not logged on, or logging out.
    NotActive,
    /// Admin messages are the session's own.
    NotApplicationMessage,
    Encode(EncodeError),
}

/// Sends application messages for an `Application`. Outbound sequence
/// numbers stay with the session.
pub struct Sender<'a, O: Outbox> {
    session: &'a mut Session,
    out: &'a mut O,
    now: i64,
}

impl<O: Outbox> Sender<'_, O> {
    /// The time the session was called with, nanoseconds since the Unix epoch.
    pub fn now(&self) -> i64 {
        self.now
    }

    /// Sends one application message; returns its MsgSeqNum.
    pub fn send(&mut self, msg_type: MsgType, body: impl FnOnce(&mut MessageWriter) -> Result<(), EncodeError>) -> Result<u32, SendError> {
        self.session.send_app(msg_type, self.now, self.out, body)
    }
}

/// Header (49, 56, 34, 52) + `body` into `buf`.
fn encode<'b>(buf: &'b mut [u8], config: &SessionConfig, msg_type: MsgType, seq: u32, now: i64,
              body: impl FnOnce(&mut MessageWriter) -> Result<(), EncodeError>) -> Result<&'b [u8], EncodeError> {
    let mut w = MessageWriter::new(buf, config.version, msg_type.as_bytes())?;
    w.header(config.our_comp_id.as_bytes(), config.their_comp_id.as_bytes(), seq, now)?;
    body(&mut w)?;
    w.finish()
}

fn parser_reason(reason: RejectReason) -> SessionRejectReason {
    match reason {
        RejectReason::InvalidTag => SessionRejectReason::InvalidTagNumber,
        RejectReason::RequiredTagMissing => SessionRejectReason::RequiredTagMissing,
        RejectReason::TagWithoutValue => SessionRejectReason::TagSpecifiedWithoutAValue,
        RejectReason::IncorrectDataFormat => SessionRejectReason::IncorrectDataFormatForValue,
        RejectReason::TagRepeated => SessionRejectReason::TagAppearsMoreThanOnce,
        RejectReason::TagOutOfOrder => SessionRejectReason::TagSpecifiedOutOfRequiredOrder,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::app::{AppMessage, NoApplication};
    use crate::fix::demo::{OneOrder, OrderSpec};
    use crate::fix::decode::Decimal;
    use crate::fix::parser::Parser;
    use crate::fix::spec::{ExecType, OrdStatus, Side};

    const T0: i64 = 1_792_947_600 * SECOND; // 2026-10-25 17:00:00 UTC
    const ORDER: &str = "11=ord-1|55=MSFT|54=1|60=20261025-17:00:00|38=100|40=2|44=412.5|";

    /// Records application messages as "MsgType:business id". ClOrdIDs starting
    /// with "bad" are refused; with `ack`, orders get an ExecutionReport.
    #[derive(Default)]
    struct Recorder {
        received: Vec<String>,
        ack: bool,
        send_results: Vec<Result<u32, SendError>>,
    }

    impl Application for Recorder {
        fn on_message<'buf, O: Outbox>(&mut self, msg: &Inbound<'_, 'buf>, send: &mut Sender<'_, O>)
            -> Result<(), BusinessReject<'buf>>
        {
            let id = msg.view.business_id().unwrap_or("");
            self.received.push(format!("{}:{id}", String::from_utf8_lossy(msg.header.msg_type)));
            if id.starts_with("bad") {
                return Err(BusinessReject::new(BusinessRejectReason::UnknownId, Some(id), "Unknown order"))
            }
            if let AppMessage::NewOrderSingle(o) = &msg.view && self.ack {
                let result = send.send(MsgType::ExecutionReport, |w| {
                    w.str(tags::ORDER_ID, "O1")?.str(tags::CL_ORD_ID, o.cl_ord_id)?.str(tags::EXEC_ID, "E1")?
                        .bytes(tags::EXEC_TYPE, ExecType::New.as_bytes())?.bytes(tags::ORD_STATUS, OrdStatus::New.as_bytes())?
                        .str(tags::SYMBOL, o.instrument.symbol)?.bytes(tags::SIDE, o.side.as_bytes())?
                        .decimal(tags::LEAVES_QTY, o.order_qty)?.u64(tags::CUM_QTY, 0)?.u64(tags::AVG_PX, 0).map(|_| ())
                });
                self.send_results.push(result);
            }
            Ok(())
        }
    }

    /// Drives a session from the counterparty's side.
    struct Peer {
        session: Session,
        parser: Box<Parser>,
        seq: u32,
        now: i64,
        sent: Vec<Vec<u8>>,
        app: Recorder,
    }

    impl Peer {
        fn acceptor() -> Self {
            let config = SessionConfig::new(Role::Acceptor, "GATEWAY", "CLIENT");
            Self { session: Session::new(config, T0), parser: Box::new(Parser::new()), seq: 1, now: T0, sent: Vec::new(), app: Recorder::default() }
        }

        fn logged_on() -> Self {
            let mut p = Self::acceptor();
            assert_eq!(p.send("A", "98=0|108=30|"), Status::Continue);
            assert_eq!(p.session.state(), SessionState::Active);
            p.sent.clear();
            p
        }

        /// Sends a message from CLIENT with the next sequence number.
        fn send(&mut self, msg_type: &str, fields: &str) -> Status {
            let seq = self.seq;
            self.seq += 1;
            self.send_with_seq(msg_type, seq, fields)
        }

        fn send_with_seq(&mut self, msg_type: &str, seq: u32, fields: &str) -> Status {
            let mut text = [0u8; 21];
            crate::fix::encoder::format_utc_timestamp(self.now, &mut text);
            let body = format!("35={msg_type}|49=CLIENT|56=GATEWAY|34={seq}|52={}|{fields}", std::str::from_utf8(&text).unwrap());
            self.send_raw(&body)
        }

        fn send_raw(&mut self, body: &str) -> Status {
            let body = body.replace('|', "\x01");
            let mut raw = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
            let sum = raw.iter().fold(0u32, |s, &b| s + b as u32) % 256;
            raw.extend(format!("10={:03}\x01", sum).bytes());
            let outcome = self.parser.parse_frame(&raw);
            self.session.on_frame(outcome, self.now, &mut self.sent, &mut self.app)
        }

        fn tick(&mut self, secs: i64) -> Status {
            self.now += secs * SECOND;
            self.session.on_timer(self.now, &mut self.sent)
        }

        /// Outbound messages as '|'-separated text, then cleared.
        fn take(&mut self) -> Vec<String> {
            self.sent.drain(..).map(|m| String::from_utf8(m).unwrap().replace('\x01', "|")).collect()
        }
    }

    fn field(msg: &str, tag: u16) -> Option<&str> {
        let prefix = format!("|{tag}=");
        let start = msg.find(&prefix)? + prefix.len();
        Some(&msg[start..start + msg[start..].find('|')?])
    }

    #[test]
    fn acceptor_logon() {
        let mut p = Peer::acceptor();
        assert_eq!(p.send("A", "98=0|108=30|"), Status::Continue);
        let out = p.take();
        assert_eq!(out.len(), 1);
        assert_eq!(field(&out[0], 35), Some("A"));
        assert_eq!((field(&out[0], 49), field(&out[0], 56)), (Some("GATEWAY"), Some("CLIENT")));
        assert_eq!((field(&out[0], 34), field(&out[0], 108)), (Some("1"), Some("30")));
        assert_eq!(p.session.state(), SessionState::Active);
        assert_eq!((p.session.next_in_seq(), p.session.next_out_seq()), (2, 2));
    }

    #[test]
    fn logon_with_reset() {
        let mut p = Peer::acceptor();
        let _ = p.send("A", "98=0|108=30|141=Y|");
        let out = p.take();
        assert_eq!(field(&out[0], 141), Some("Y"));
    }

    #[test]
    fn first_message_must_be_logon() {
        let mut p = Peer::acceptor();
        assert_eq!(p.send("0", ""), Status::Disconnect);
        assert!(p.take().is_empty());
    }

    #[test]
    fn invalid_logon_gets_logout() {
        let mut p = Peer::acceptor();
        assert_eq!(p.send("A", "98=0|"), Status::Disconnect);
        let out = p.take();
        assert_eq!((out.len(), field(&out[0], 35)), (1, Some("5")));
        let mut p = Peer::acceptor();
        assert_eq!(p.send("A", "98=2|108=30|"), Status::Disconnect);
        assert_eq!(field(&p.take()[0], 58), Some("Encryption not supported"));
    }

    #[test]
    fn logon_timeout() {
        let mut p = Peer::acceptor();
        assert_eq!(p.tick(5), Status::Continue);
        assert_eq!(p.tick(5), Status::Disconnect);
    }

    #[test]
    fn heartbeats_and_test_request() {
        let mut p = Peer::logged_on();
        assert_eq!(p.tick(29), Status::Continue);
        assert!(p.take().is_empty());
        // 30 s without sending: heartbeat.
        let _ = p.send("0", "");
        assert_eq!(p.tick(1), Status::Continue);
        let out = p.take();
        assert_eq!((out.len(), field(&out[0], 35)), (1, Some("0")));

        // 36 s of silence from them: TestRequest.
        assert_eq!(p.tick(35), Status::Continue);
        let out = p.take();
        let test_req = out.iter().find(|m| field(m, 35) == Some("1")).expect("TestRequest");
        let id = field(test_req, 112).unwrap().to_string();

        // Answering it clears the pending test.
        let _ = p.send("0", &format!("112={id}|"));
        assert_eq!(p.tick(40), Status::Continue);
        assert!(p.take().iter().any(|m| field(m, 35) == Some("1")), "new TestRequest after more silence");
    }

    #[test]
    fn unanswered_test_request_logs_out() {
        let mut p = Peer::logged_on();
        let _ = p.tick(36);
        assert!(p.take().iter().any(|m| field(m, 35) == Some("1")));
        assert_eq!(p.tick(30), Status::Disconnect);
        assert_eq!(field(&p.take().last().unwrap().clone(), 35), Some("5"));
    }

    #[test]
    fn answers_test_request() {
        let mut p = Peer::logged_on();
        let _ = p.send("1", "112=abc|");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 112)), (Some("0"), Some("abc")));
    }

    #[test]
    fn gap_triggers_one_resend_request() {
        let mut p = Peer::logged_on();
        p.seq = 5;
        let _ = p.send("0", "");
        let out = p.take();
        assert_eq!(field(&out[0], 35), Some("2"));
        assert_eq!((field(&out[0], 7), field(&out[0], 16)), (Some("2"), Some("0")));
        assert_eq!(p.session.next_in_seq(), 2);
        // Still in the gap: no second request.
        let _ = p.send("0", "");
        assert!(p.take().is_empty());

        // Their gap fill closes it.
        let _ = p.send_with_seq("4", 2, "43=Y|122=20261025-16:59:00|123=Y|36=7|");
        assert_eq!(p.session.next_in_seq(), 7);
        assert_eq!(p.send_with_seq("0", 7, ""), Status::Continue);
        assert_eq!(p.session.next_in_seq(), 8);
    }

    #[test]
    fn too_low_seq() {
        let mut p = Peer::logged_on();
        let _ = p.send("0", "");
        // Duplicate with PossDup: ignored.
        assert_eq!(p.send_with_seq("0", 2, "43=Y|122=20261025-16:59:00|"), Status::Continue);
        assert!(p.take().is_empty());
        // Without PossDup: logout.
        assert_eq!(p.send_with_seq("0", 2, ""), Status::Disconnect);
        assert_eq!(field(&p.take()[0], 58), Some("MsgSeqNum too low"));
    }

    #[test]
    fn answers_resend_request_with_gap_fill() {
        let mut p = Peer::logged_on();
        let _ = p.send("1", "112=a|");
        let _ = p.send("1", "112=b|");
        p.take();
        // We've sent 1 (Logon), 2 and 3 (Heartbeats); next is 4.
        let _ = p.send("2", "7=2|16=0|");
        let out = p.take();
        assert_eq!(out.len(), 1);
        let fill = &out[0];
        assert_eq!(field(fill, 35), Some("4"));
        assert_eq!((field(fill, 34), field(fill, 36)), (Some("2"), Some("4")));
        assert_eq!((field(fill, 43), field(fill, 123)), (Some("Y"), Some("Y")));
        assert!(field(fill, 122).is_some());
        assert_eq!(p.session.next_out_seq(), 4);
    }

    #[test]
    fn sequence_reset_lowering_is_rejected() {
        let mut p = Peer::logged_on();
        let _ = p.send("0", "");
        let _ = p.send("4", "123=Y|36=2|");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 373), field(&out[0], 371)), (Some("3"), Some("5"), Some("36")));
        // Reset mode ignores MsgSeqNum.
        let _ = p.send_with_seq("4", 999, "36=50|");
        assert_eq!(p.session.next_in_seq(), 50);
    }

    #[test]
    fn invalid_body_is_rejected_and_consumes_seq() {
        let mut p = Peer::logged_on();
        let _ = p.send("1", "");
        let out = p.take();
        assert_eq!(field(&out[0], 35), Some("3"));
        assert_eq!((field(&out[0], 45), field(&out[0], 371), field(&out[0], 373)), (Some("2"), Some("112"), Some("1")));
        assert_eq!(p.session.next_in_seq(), 3);
        assert_eq!(p.send("0", ""), Status::Continue);
        assert!(p.take().is_empty());
    }

    #[test]
    fn malformed_field_is_rejected() {
        let mut p = Peer::logged_on();
        let _ = p.send("0", "58=|");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 45), field(&out[0], 373)), (Some("3"), Some("2"), Some("4")));
        assert_eq!(p.session.next_in_seq(), 3);
    }

    #[test]
    fn comp_id_problem() {
        let mut p = Peer::logged_on();
        assert_eq!(p.send_raw("35=0|49=MALLORY|56=GATEWAY|34=2|52=20261025-17:00:00.000|"), Status::Disconnect);
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 373)), (Some("3"), Some("9")));
        assert_eq!(field(&out[1], 35), Some("5"));
    }

    #[test]
    fn counterparty_logout() {
        let mut p = Peer::logged_on();
        assert_eq!(p.send("5", "58=bye|"), Status::Disconnect);
        assert_eq!(field(&p.take()[0], 35), Some("5"));
        assert_eq!(p.session.state(), SessionState::Closed);
    }

    #[test]
    fn our_logout() {
        let mut p = Peer::logged_on();
        assert_eq!(p.session.logout("done", p.now, &mut p.sent), Status::Continue);
        assert_eq!(field(&p.take()[0], 58), Some("done"));
        assert_eq!(p.session.state(), SessionState::LogoutSent);
        assert_eq!(p.send("5", ""), Status::Disconnect);
        assert!(p.take().is_empty(), "no second Logout");
    }

    #[test]
    fn logout_timeout() {
        let mut p = Peer::logged_on();
        let _ = p.session.logout("done", p.now, &mut p.sent);
        assert_eq!(p.tick(10), Status::Disconnect);
    }

    #[test]
    fn second_logon_logs_out() {
        let mut p = Peer::logged_on();
        assert_eq!(p.send("A", "98=0|108=30|"), Status::Disconnect);
    }

    #[test]
    fn initiator() {
        let mut config = SessionConfig::new(Role::Initiator, "CLIENT", "GATEWAY");
        config.reset_on_logon = true;
        let mut session = Session::new(config, T0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        assert_eq!(session.start(T0, &mut out), Status::Continue);
        assert_eq!(session.state(), SessionState::LogonSent);
        let logon = String::from_utf8(out.pop().unwrap()).unwrap().replace('\x01', "|");
        assert_eq!((field(&logon, 35), field(&logon, 141), field(&logon, 108)), (Some("A"), Some("Y"), Some("30")));

        // The gateway's reply.
        let body = "35=A|49=GATEWAY|56=CLIENT|34=1|52=20261025-17:00:00.000|98=0|108=30|141=Y|".replace('|', "\x01");
        let mut raw = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = raw.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        raw.extend(format!("10={:03}\x01", sum).bytes());
        let mut parser = Box::new(Parser::new());
        assert_eq!(session.on_frame(parser.parse_frame(&raw), T0, &mut out, &mut NoApplication), Status::Continue);
        assert_eq!(session.state(), SessionState::Active);
        assert!(out.is_empty(), "initiator doesn't answer the Logon reply");
        assert_eq!((session.next_in_seq(), session.next_out_seq()), (2, 2));
    }

    fn logon_reply() -> Vec<u8> {
        let body = "35=A|49=GATEWAY|56=CLIENT|34=1|52=20261025-17:00:00.000|98=0|108=30|".replace('|', "\x01");
        let mut raw = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = raw.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        raw.extend(format!("10={:03}\x01", sum).bytes());
        raw
    }

    #[test]
    fn application_messages_reach_the_application() {
        let mut p = Peer::logged_on();
        assert_eq!(p.send("D", ORDER), Status::Continue);
        assert_eq!(p.send("F", "41=ord-1|11=ord-2|55=MSFT|54=1|60=20261025-17:00:00|"), Status::Continue);
        assert_eq!(p.send("j", "45=1|372=8|380=0|"), Status::Continue);
        assert_eq!(p.app.received, ["D:ord-1", "F:ord-2", "j:"]);
        assert!(p.take().is_empty());
        assert_eq!(p.session.next_in_seq(), 5);
    }

    #[test]
    fn application_refusal_is_a_business_message_reject() {
        let mut p = Peer::logged_on();
        let _ = p.send("D", &ORDER.replace("ord-1", "bad-7"));
        let out = p.take();
        assert_eq!(out.len(), 1);
        let j = &out[0];
        assert_eq!((field(j, 35), field(j, 45), field(j, 372)), (Some("j"), Some("2"), Some("D")));
        assert_eq!((field(j, 379), field(j, 380), field(j, 58)), (Some("bad-7"), Some("1"), Some("Unknown order")));
        assert_eq!((p.session.next_in_seq(), p.session.next_out_seq()), (3, 3));
    }

    #[test]
    fn business_rules_are_checked_before_the_application() {
        let mut p = Peer::logged_on();
        // Limit order without a price.
        let _ = p.send("D", "11=ord-1|55=MSFT|54=1|60=20261025-17:00:00|38=100|40=2|");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 380), field(&out[0], 379)), (Some("j"), Some("5"), Some("ord-1")));
        assert!(p.app.received.is_empty());
        assert_eq!(p.session.next_in_seq(), 3);
    }

    #[test]
    fn schema_errors_in_application_messages_are_session_rejects() {
        let mut p = Peer::logged_on();
        let _ = p.send("D", "11=ord-1|55=MSFT|60=20261025-17:00:00|38=100|40=1|");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 371), field(&out[0], 373)), (Some("3"), Some("54"), Some("1")));
        assert!(p.app.received.is_empty());
        assert_eq!(p.session.next_in_seq(), 3);
    }

    #[test]
    fn unsupported_application_message() {
        let mut p = Peer::logged_on();
        let raw = {
            let body = "35=D|49=CLIENT|56=GATEWAY|34=2|52=20261025-17:00:00.000|11=ord-1|55=MSFT|54=1|60=20261025-17:00:00|38=1|40=1|".replace('|', "\x01");
            let mut raw = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
            let sum = raw.iter().fold(0u32, |s, &b| s + b as u32) % 256;
            raw.extend(format!("10={:03}\x01", sum).bytes());
            raw
        };
        let outcome = p.parser.parse_frame(&raw);
        let _ = p.session.on_frame(outcome, p.now, &mut p.sent, &mut NoApplication);
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 380), field(&out[0], 379)), (Some("j"), Some("3"), Some("ord-1")));
    }

    #[test]
    fn application_replies_through_the_session() {
        let mut p = Peer::logged_on();
        p.app.ack = true;
        let _ = p.send("D", ORDER);
        assert_eq!(p.app.send_results, [Ok(2)]);
        let out = p.take();
        let er = &out[0];
        assert_eq!((field(er, 35), field(er, 34), field(er, 49), field(er, 56)), (Some("8"), Some("2"), Some("GATEWAY"), Some("CLIENT")));
        assert_eq!((field(er, 11), field(er, 150), field(er, 151)), (Some("ord-1"), Some("0"), Some("100")));
        assert_eq!(p.session.next_out_seq(), 3);
    }

    #[test]
    fn sender_rules() {
        let mut p = Peer::acceptor();
        let order = |w: &mut MessageWriter| w.str(tags::CL_ORD_ID, "x").map(|_| ());
        // Not logged on yet.
        assert_eq!(p.session.send_app(MsgType::NewOrderSingle, T0, &mut p.sent, order), Err(SendError::NotActive));
        let _ = p.send("A", "98=0|108=30|");
        p.take();
        // Admin messages belong to the session.
        assert_eq!(p.session.send_app(MsgType::Heartbeat, T0, &mut p.sent, |_| Ok(())), Err(SendError::NotApplicationMessage));
        // An encoding error sends nothing and keeps the sequence number.
        let next = p.session.next_out_seq();
        assert_eq!(p.session.send_app(MsgType::NewOrderSingle, T0, &mut p.sent, |w| w.str(tags::TEXT, "a\x01b").map(|_| ())),
            Err(SendError::Encode(EncodeError::InvalidValue { tag: tags::TEXT })));
        assert_eq!(p.session.send_app(MsgType::NewOrderSingle, T0, &mut p.sent, |w| w.str(tags::TEXT, &"x".repeat(OUT_BUF)).map(|_| ())),
            Err(SendError::Encode(EncodeError::BufferFull)));
        assert!(p.take().is_empty());
        assert_eq!(p.session.send_app(MsgType::NewOrderSingle, T0, &mut p.sent, order), Ok(next));
        assert_eq!(field(&p.take()[0], 34), Some(next.to_string().as_str()));
        // Logging out: no more application messages.
        let _ = p.session.logout("done", T0, &mut p.sent);
        assert_eq!(p.session.send_app(MsgType::NewOrderSingle, T0, &mut p.sent, order), Err(SendError::NotActive));
    }

    #[test]
    fn possdup_application_message_is_not_delivered_twice() {
        let mut p = Peer::logged_on();
        let _ = p.send("D", ORDER);
        assert_eq!(p.send_with_seq("D", 2, &format!("43=Y|122=20261025-16:59:00|{ORDER}")), Status::Continue);
        assert_eq!(p.app.received, ["D:ord-1"]);
        assert!(p.take().is_empty());
    }

    #[test]
    fn application_messages_wait_for_the_resend() {
        let mut p = Peer::logged_on();
        p.seq = 4;
        let _ = p.send("D", ORDER);
        assert!(p.app.received.is_empty(), "out of order: held back");
        assert_eq!(field(&p.take()[0], 35), Some("2"));
    }

    #[test]
    fn unencodable_business_reject_falls_back_to_the_required_fields() {
        let mut p = Peer::logged_on();
        // A ClOrdID too long for the reject's 4 KB buffer together with the text.
        let long = format!("bad-{}", "x".repeat(OUT_BUF));
        let _ = p.send("D", &ORDER.replace("ord-1", &long));
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 380)), (Some("j"), Some("1")));
        assert_eq!((field(&out[0], 379), field(&out[0], 58)), (None, None));
    }

    #[test]
    fn initiator_application_sends_on_logon() {
        let config = SessionConfig::new(Role::Initiator, "CLIENT", "GATEWAY");
        let mut session = Session::new(config, T0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut app = OneOrder::new(OrderSpec {
            cl_ord_id: "c1".into(), symbol: "MSFT".into(), side: Side::Sell, qty: Decimal::new(5, 0), price: Some(Decimal::new(41250, 2)),
        });
        let _ = session.start(T0, &mut out);
        out.clear();
        let mut parser = Box::new(Parser::new());
        let raw = logon_reply();
        assert_eq!(session.on_frame(parser.parse_frame(&raw), T0, &mut out, &mut app), Status::Continue);
        let order = String::from_utf8(out.pop().expect("order sent")).unwrap().replace('\x01', "|");
        assert_eq!((field(&order, 35), field(&order, 34), field(&order, 11)), (Some("D"), Some("2"), Some("c1")));
        assert_eq!((field(&order, 54), field(&order, 38), field(&order, 40), field(&order, 44)), (Some("2"), Some("5"), Some("2"), Some("412.50")));
        assert_eq!(session.next_out_seq(), 3);
    }

    #[test]
    fn defined_but_unsupported_msg_type_is_a_business_message_reject() {
        let mut p = Peer::logged_on();
        // MarketDataRequest: FIX 4.4, not in the dictionary.
        assert_eq!(p.send("V", "262=md-1|263=0|264=1|"), Status::Continue);
        let out = p.take();
        assert_eq!(out.len(), 1);
        let j = &out[0];
        assert_eq!((field(j, 35), field(j, 45), field(j, 372), field(j, 380)), (Some("j"), Some("2"), Some("V"), Some("3")));
        assert_eq!((field(j, 379), field(j, 58)), (None, Some("Unsupported message type")));
        assert!(p.app.received.is_empty());
        assert_eq!((p.session.next_in_seq(), p.session.next_out_seq()), (3, 3));
        // User-defined types too.
        let _ = p.send("U42", "");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 372), field(&out[0], 380)), (Some("j"), Some("U42"), Some("3")));
    }

    #[test]
    fn undefined_msg_type_is_a_session_reject() {
        let mut p = Peer::logged_on();
        let _ = p.send("ZZ", "");
        let out = p.take();
        assert_eq!((field(&out[0], 35), field(&out[0], 371), field(&out[0], 372), field(&out[0], 373)), (Some("3"), Some("35"), Some("ZZ"), Some("11")));
        assert_eq!(p.session.next_in_seq(), 3);
    }

    #[test]
    fn unsupported_msg_type_waits_for_the_resend_like_any_other() {
        let mut p = Peer::logged_on();
        p.seq = 4;
        let _ = p.send("V", "");
        let out = p.take();
        assert_eq!((out.len(), field(&out[0], 35)), (1, Some("2")));
    }
}
