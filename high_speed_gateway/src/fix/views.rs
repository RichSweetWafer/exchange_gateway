//! Typed views of the session-level (admin) messages. Application messages
//! are in app.rs.
//!
//! `decode` runs the strict schema check (`validate_body`), then builds the view
//! and applies the checks the schema can't express (e.g. EndSeqNo >= BeginSeqNo).
//! The header has already been checked by `validate_header`.

use super::decode;
use super::message::{Group, Message};
use super::spec::{self, EncryptMethod, MsgType, SessionRejectReason, tags};
use super::validate::{Header, SessionReject, validate_body};

/// Heartbeat (0).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Heartbeat<'buf> {
    /// Set when answering a TestRequest.
    pub test_req_id: Option<&'buf str>,
}

/// TestRequest (1).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct TestRequest<'buf> {
    pub test_req_id: &'buf str,
}

/// ResendRequest (2).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ResendRequest {
    pub begin_seq_no: u32,
    /// 0 means "everything after BeginSeqNo".
    pub end_seq_no: u32,
}

/// Reject (3), received from the counterparty.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Reject<'buf> {
    pub ref_seq_num: u32,
    pub ref_tag_id: Option<u16>,
    pub ref_msg_type: Option<&'buf [u8]>,
    pub reason: Option<SessionRejectReason>,
    pub text: Option<&'buf str>,
}

/// SequenceReset (4).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SequenceReset {
    /// Gap fill (true) or reset mode (false).
    pub gap_fill: bool,
    pub new_seq_no: u32,
}

/// Logout (5).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Logout<'buf> {
    pub text: Option<&'buf str>,
}

/// Logon (A).
#[derive(Clone, Copy)]
pub struct Logon<'buf> {
    pub encrypt_method: EncryptMethod,
    /// Seconds.
    pub heart_bt_int: u32,
    pub reset_seq_num: bool,
    pub next_expected_msg_seq_num: Option<u32>,
    pub max_message_size: Option<u32>,
    pub test_message: bool,
    pub username: Option<&'buf str>,
    pub password: Option<&'buf str>,
    /// NoMsgTypes (384): RefMsgType (372) + MsgDirection (385) per entry.
    pub msg_types: Option<Group<'buf>>,
}

impl std::fmt::Debug for Logon<'_> {
    // Keeps the password out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logon")
            .field("encrypt_method", &self.encrypt_method)
            .field("heart_bt_int", &self.heart_bt_int)
            .field("reset_seq_num", &self.reset_seq_num)
            .field("next_expected_msg_seq_num", &self.next_expected_msg_seq_num)
            .field("max_message_size", &self.max_message_size)
            .field("test_message", &self.test_message)
            .field("username", &self.username)
            .field("password", &self.password.map(|_| "<redacted>"))
            .field("msg_types", &self.msg_types.map(|g| g.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SessionMessage<'buf> {
    Heartbeat(Heartbeat<'buf>),
    TestRequest(TestRequest<'buf>),
    ResendRequest(ResendRequest),
    Reject(Reject<'buf>),
    SequenceReset(SequenceReset),
    Logout(Logout<'buf>),
    Logon(Logon<'buf>),
}

/// Validates the message against its spec and builds the typed view.
pub fn decode<'buf>(msg: &Message<'buf>, header: &Header<'buf>) -> Result<SessionMessage<'buf>, SessionReject<'buf>> {
    let spec = validate_body(msg, header)?;

    let reject = |tag: u16, reason: SessionRejectReason, text: &'static str| SessionReject {
        ref_seq_num: Some(header.msg_seq_num),
        ref_tag: Some(tag),
        ref_msg_type: Some(header.msg_type),
        reason,
        text,
    };
    if !spec.admin {
        // Application messages go through app::decode.
        return Err(reject(tags::MSG_TYPE, SessionRejectReason::InvalidMsgType, "Not a session message"))
    }
    // The schema check guarantees format and presence of required fields, so
    // these only fail for values out of the u32 range.
    let seq = |tag: u16| -> Result<Option<u32>, SessionReject<'buf>> {
        match msg.fields.get_slice(tag, msg.raw) {
            None => Ok(None),
            Some(v) => decode::to_u32(v).map(Some).ok_or(reject(tag, SessionRejectReason::ValueIsIncorrect, "Value out of range")),
        }
    };
    let required_seq = |tag: u16| seq(tag).map(|v| v.expect("required tag checked by the schema"));
    let flag = |tag: u16| msg.get_boolean(tag).unwrap_or(false);

    let msg_type = MsgType::from_bytes(header.msg_type).expect("validate_body accepted the MsgType");
    Ok(match msg_type {
        MsgType::Heartbeat => SessionMessage::Heartbeat(Heartbeat { test_req_id: msg.get_str(tags::TEST_REQ_ID) }),

        MsgType::TestRequest => SessionMessage::TestRequest(TestRequest {
            test_req_id: msg.get_str(tags::TEST_REQ_ID).expect("required"),
        }),

        MsgType::ResendRequest => {
            let begin_seq_no = required_seq(tags::BEGIN_SEQ_NO)?;
            let end_seq_no = required_seq(tags::END_SEQ_NO)?;
            if begin_seq_no == 0 {
                return Err(reject(tags::BEGIN_SEQ_NO, SessionRejectReason::ValueIsIncorrect, "BeginSeqNo must be positive"))
            }
            if end_seq_no != 0 && end_seq_no < begin_seq_no {
                return Err(reject(tags::END_SEQ_NO, SessionRejectReason::ValueIsIncorrect, "EndSeqNo before BeginSeqNo"))
            }
            SessionMessage::ResendRequest(ResendRequest { begin_seq_no, end_seq_no })
        }

        MsgType::Reject => SessionMessage::Reject(Reject {
            ref_seq_num: required_seq(tags::REF_SEQ_NUM)?,
            ref_tag_id: msg.fields.get_slice(tags::REF_TAG_ID, msg.raw).and_then(decode::to_u16),
            ref_msg_type: msg.fields.get_slice(tags::REF_MSG_TYPE, msg.raw),
            reason: msg.fields.get_slice(tags::SESSION_REJECT_REASON, msg.raw).and_then(SessionRejectReason::from_bytes),
            text: msg.get_str(tags::TEXT),
        }),

        MsgType::SequenceReset => {
            let new_seq_no = required_seq(tags::NEW_SEQ_NO)?;
            if new_seq_no == 0 {
                return Err(reject(tags::NEW_SEQ_NO, SessionRejectReason::ValueIsIncorrect, "NewSeqNo must be positive"))
            }
            SessionMessage::SequenceReset(SequenceReset { gap_fill: flag(tags::GAP_FILL_FLAG), new_seq_no })
        }

        MsgType::Logout => SessionMessage::Logout(Logout { text: msg.get_str(tags::TEXT) }),

        MsgType::Logon => {
            let encrypt_method = msg.fields.get_slice(tags::ENCRYPT_METHOD, msg.raw)
                .and_then(EncryptMethod::from_bytes)
                .expect("enum checked by the schema");
            let heart_bt_int = required_seq(tags::HEART_BT_INT)?;
            let msg_types = msg.group(&spec::LOGON_NO_MSG_TYPES).expect("group checked by the schema");
            SessionMessage::Logon(Logon {
                encrypt_method,
                heart_bt_int,
                reset_seq_num: flag(tags::RESET_SEQ_NUM_FLAG),
                next_expected_msg_seq_num: seq(tags::NEXT_EXPECTED_MSG_SEQ_NUM)?,
                max_message_size: seq(tags::MAX_MESSAGE_SIZE)?,
                test_message: flag(tags::TEST_MESSAGE_INDICATOR),
                username: msg.get_str(tags::USERNAME),
                password: msg.get_str(tags::PASSWORD),
                msg_types,
            })
        }

        _ => unreachable!("application MsgTypes returned above"),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{ParseOutcome, Parser};
    use crate::fix::validate::{HeaderRules, validate_header};

    const SECOND: i64 = 1_000_000_000;
    const NOW: i64 = 1_792_947_600 * SECOND; // 2026-10-25 17:00:00 UTC
    const RULES: HeaderRules = HeaderRules { our_comp_id: b"GATEWAY", their_comp_id: b"CLIENT", max_sending_time_skew_ns: 120 * SECOND };

    fn frame(msg_type: &str, fields: &str) -> Vec<u8> {
        let body = format!("35={msg_type}|49=CLIENT|56=GATEWAY|34=7|52=20261025-17:00:00.000|{fields}").replace('|', "\x01");
        let mut out = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = out.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        out.extend(format!("10={:03}\x01", sum).bytes());
        out
    }

    fn with_view<R>(msg_type: &str, fields: &str, check: impl FnOnce(Result<SessionMessage, SessionReject>) -> R) -> R {
        let raw = frame(msg_type, fields);
        let mut parser = Box::new(Parser::new());
        let ParseOutcome::Message(msg) = parser.parse_frame(&raw) else { panic!("didn't parse") };
        let header = validate_header(&msg, &RULES, NOW).expect("header ok");
        check(decode(&msg, &header))
    }

    fn reject_of(msg_type: &str, fields: &str) -> (Option<u16>, SessionRejectReason) {
        with_view(msg_type, fields, |r| {
            let r = r.expect_err("should reject");
            (r.ref_tag, r.reason)
        })
    }

    #[test]
    fn heartbeat_and_test_request() {
        with_view("0", "", |v| assert!(matches!(v.unwrap(), SessionMessage::Heartbeat(Heartbeat { test_req_id: None }))));
        with_view("0", "112=t1|", |v| assert!(matches!(v.unwrap(), SessionMessage::Heartbeat(Heartbeat { test_req_id: Some("t1") }))));
        with_view("1", "112=t2|", |v| assert!(matches!(v.unwrap(), SessionMessage::TestRequest(TestRequest { test_req_id: "t2" }))));
    }

    #[test]
    fn resend_request() {
        with_view("2", "7=5|16=0|", |v| assert!(matches!(v.unwrap(), SessionMessage::ResendRequest(ResendRequest { begin_seq_no: 5, end_seq_no: 0 }))));
        with_view("2", "7=5|16=9|", |v| assert!(matches!(v.unwrap(), SessionMessage::ResendRequest(ResendRequest { begin_seq_no: 5, end_seq_no: 9 }))));
        assert_eq!(reject_of("2", "7=9|16=5|"), (Some(16), SessionRejectReason::ValueIsIncorrect));
        assert_eq!(reject_of("2", "7=0|16=5|"), (Some(7), SessionRejectReason::ValueIsIncorrect));
        assert_eq!(reject_of("2", "7=99999999999|16=0|"), (Some(7), SessionRejectReason::ValueIsIncorrect));
    }

    #[test]
    fn reject() {
        with_view("3", "45=3|371=55|372=D|373=5|58=bad price|", |v| {
            let SessionMessage::Reject(r) = v.unwrap() else { panic!() };
            assert_eq!(r, Reject {
                ref_seq_num: 3,
                ref_tag_id: Some(55),
                ref_msg_type: Some(b"D"),
                reason: Some(SessionRejectReason::ValueIsIncorrect),
                text: Some("bad price"),
            });
        });
    }

    #[test]
    fn sequence_reset() {
        with_view("4", "123=Y|36=20|", |v| assert!(matches!(v.unwrap(), SessionMessage::SequenceReset(SequenceReset { gap_fill: true, new_seq_no: 20 }))));
        with_view("4", "36=20|", |v| assert!(matches!(v.unwrap(), SessionMessage::SequenceReset(SequenceReset { gap_fill: false, .. }))));
        assert_eq!(reject_of("4", "36=0|"), (Some(36), SessionRejectReason::ValueIsIncorrect));
    }

    #[test]
    fn logout() {
        with_view("5", "58=bye|", |v| assert!(matches!(v.unwrap(), SessionMessage::Logout(Logout { text: Some("bye") }))));
    }

    #[test]
    fn logon() {
        with_view("A", "98=0|108=30|141=Y|789=5|384=2|372=D|385=S|372=8|385=R|553=user|554=secret|", |v| {
            let SessionMessage::Logon(l) = v.unwrap() else { panic!() };
            assert_eq!(l.encrypt_method, EncryptMethod::NoneOther);
            assert_eq!(l.heart_bt_int, 30);
            assert!(l.reset_seq_num && !l.test_message);
            assert_eq!(l.next_expected_msg_seq_num, Some(5));
            assert_eq!((l.username, l.password), (Some("user"), Some("secret")));
            let types: Vec<_> = l.msg_types.unwrap().entries().map(|e| (e.get_str(372), e.get_str(385))).collect();
            assert_eq!(types, [(Some("D"), Some("S")), (Some("8"), Some("R"))]);
            assert!(!format!("{l:?}").contains("secret"));
        });
        with_view("A", "98=0|108=30|", |v| {
            let SessionMessage::Logon(l) = v.unwrap() else { panic!() };
            assert!(l.msg_types.is_none() && !l.reset_seq_num);
        });
    }

    #[test]
    fn schema_errors_surface_from_decode() {
        assert_eq!(reject_of("A", "98=0|"), (Some(108), SessionRejectReason::RequiredTagMissing));
        assert_eq!(reject_of("0", "108=30|"), (Some(108), SessionRejectReason::TagNotDefinedForThisMessageType));
    }
}
