//! Message validation, in two steps:
//! 1. `validate_header`: StandardHeader rules every message shares (MsgSeqNum,
//!    CompIDs, SendingTime, PossDup). Run by the session layer first.
//! 2. `validate_body`: strict schema check of the whole message against the
//!    generated spec (allowed tags, order, repeats, groups, required tags,
//!    value formats). Run before the message reaches a view.
//!
//! Errors carry what a Reject (35=3) needs. Sequence number gaps and resend
//! handling are session state and live in the session, not here.

use super::message::{Entry, Message};
use super::spec::{self, FieldType, GroupSpec, MessageSpec, SessionRejectReason, tags};
use super::decode;

/// Everything needed to send a session-level Reject (35=3).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct SessionReject<'buf> {
    /// RefSeqNum (45). None only if MsgSeqNum itself was unreadable.
    pub ref_seq_num: Option<u32>,
    /// RefTagID (371).
    pub ref_tag: Option<u16>,
    /// RefMsgType (372).
    pub ref_msg_type: Option<&'buf [u8]>,
    /// SessionRejectReason (373).
    pub reason: SessionRejectReason,
    /// Text (58).
    pub text: &'static str,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HeaderError<'buf> {
    /// Send a Reject. The message still consumes its sequence number.
    Reject(SessionReject<'buf>),
    /// Send a Reject, then Logout and disconnect (CompID and SendingTime problems).
    RejectAndLogout(SessionReject<'buf>),
    /// Logout and disconnect without a Reject (MsgSeqNum missing or unreadable).
    Logout { text: &'static str },
}

/// What the header must match for this session.
#[derive(Debug, Clone, Copy)]
pub struct HeaderRules<'a> {
    /// Ours: incoming TargetCompID (56) must equal it.
    pub our_comp_id: &'a [u8],
    /// The counterparty's: incoming SenderCompID (49) must equal it.
    pub their_comp_id: &'a [u8],
    /// Allowed difference between SendingTime and our clock.
    pub max_sending_time_skew_ns: i64,
}

/// Validated StandardHeader.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Header<'buf> {
    pub msg_type: &'buf [u8],
    pub sender_comp_id: &'buf [u8],
    pub target_comp_id: &'buf [u8],
    pub msg_seq_num: u32,
    /// Nanoseconds since the Unix epoch.
    pub sending_time: i64,
    pub poss_dup: bool,
    pub poss_resend: bool,
    pub orig_sending_time: Option<i64>,
}

/// Checks the StandardHeader. `now_ns` is our clock, in nanoseconds since the
/// Unix epoch. BeginString, BodyLength and MsgType were already checked by the
/// framer and parser.
pub fn validate_header<'buf>(msg: &Message<'buf>, rules: &HeaderRules, now_ns: i64) -> Result<Header<'buf>, HeaderError<'buf>> {
    let msg_type = msg.get_msg_type().unwrap_or(b"");

    // Without a sequence number there's nothing to reference in a Reject.
    let msg_seq_num = match msg.fields.get_slice(tags::MSG_SEQ_NUM, msg.raw).map(decode::to_u32) {
        Some(Some(n)) if n > 0 => n,
        _ => return Err(HeaderError::Logout { text: "MsgSeqNum(34) missing or invalid" }),
    };
    let reject = |tag: u16, reason: SessionRejectReason, text: &'static str| SessionReject {
        ref_seq_num: Some(msg_seq_num),
        ref_tag: Some(tag),
        ref_msg_type: Some(msg_type),
        reason,
        text,
    };

    for &tag in spec::HEADER.required {
        if msg.fields.position(tag).is_none() {
            return Err(HeaderError::Reject(reject(tag, SessionRejectReason::RequiredTagMissing, "Required header field missing")))
        }
    }

    let sender_comp_id = msg.fields.get_slice(tags::SENDER_COMP_ID, msg.raw).unwrap_or_default();
    if sender_comp_id != rules.their_comp_id {
        return Err(HeaderError::RejectAndLogout(reject(tags::SENDER_COMP_ID, SessionRejectReason::CompIdProblem, "Unexpected SenderCompID")))
    }
    let target_comp_id = msg.fields.get_slice(tags::TARGET_COMP_ID, msg.raw).unwrap_or_default();
    if target_comp_id != rules.our_comp_id {
        return Err(HeaderError::RejectAndLogout(reject(tags::TARGET_COMP_ID, SessionRejectReason::CompIdProblem, "Unexpected TargetCompID")))
    }

    let sending_time = msg.fields.get_slice(tags::SENDING_TIME, msg.raw).and_then(parse_utc_timestamp)
        .ok_or(HeaderError::Reject(reject(tags::SENDING_TIME, SessionRejectReason::IncorrectDataFormatForValue, "Invalid SendingTime")))?;
    if (now_ns - sending_time).abs() > rules.max_sending_time_skew_ns {
        return Err(HeaderError::RejectAndLogout(reject(tags::SENDING_TIME, SessionRejectReason::SendingTimeAccuracyProblem, "SendingTime accuracy problem")))
    }

    let flag = |tag: u16| match msg.fields.get_slice(tag, msg.raw) {
        None | Some(b"N") => Ok(false),
        Some(b"Y") => Ok(true),
        Some(_) => Err(HeaderError::Reject(reject(tag, SessionRejectReason::IncorrectDataFormatForValue, "Invalid boolean"))),
    };
    let poss_dup = flag(tags::POSS_DUP_FLAG)?;
    let poss_resend = flag(tags::POSS_RESEND)?;

    let orig_sending_time = match msg.fields.get_slice(tags::ORIG_SENDING_TIME, msg.raw) {
        None => None,
        Some(v) => Some(parse_utc_timestamp(v)
            .ok_or(HeaderError::Reject(reject(tags::ORIG_SENDING_TIME, SessionRejectReason::IncorrectDataFormatForValue, "Invalid OrigSendingTime")))?),
    };
    if poss_dup {
        match orig_sending_time {
            None => return Err(HeaderError::Reject(reject(tags::ORIG_SENDING_TIME, SessionRejectReason::RequiredTagMissing, "PossDupFlag=Y without OrigSendingTime"))),
            Some(orig) if orig > sending_time => {
                return Err(HeaderError::RejectAndLogout(reject(tags::ORIG_SENDING_TIME, SessionRejectReason::SendingTimeAccuracyProblem, "OrigSendingTime later than SendingTime")))
            }
            Some(_) => {}
        }
    }

    Ok(Header { msg_type, sender_comp_id, target_comp_id, msg_seq_num, sending_time, poss_dup, poss_resend, orig_sending_time })
}


/// Strict schema check of the whole message: every tag must belong to the
/// header, this message type or the trailer, in that order; no repeats outside
/// groups; groups well formed; required body tags present; values well formed.
/// Returns the message's spec for the view.
pub fn validate_body<'buf>(msg: &Message<'buf>, header: &Header<'buf>) -> Result<&'static MessageSpec, SessionReject<'buf>> {
    let reject = |tag: u16, reason: SessionRejectReason, text: &'static str| SessionReject {
        ref_seq_num: Some(header.msg_seq_num),
        ref_tag: Some(tag),
        ref_msg_type: Some(header.msg_type),
        reason,
        text,
    };
    let spec = spec::message(header.msg_type)
        .ok_or(reject(tags::MSG_TYPE, SessionRejectReason::InvalidMsgType, "Unsupported MsgType"))?;

    #[derive(PartialEq, PartialOrd)]
    enum Section { Header, Body, Trailer }
    let mut section = Section::Header;

    let tape = msg.fields.tape();
    let mut i = 0;
    while i < tape.len() {
        let tag = tape[i].tag;

        // Group members aren't in the section sets, so one found outside its
        // group ends up as "not defined for this message type".
        let here = if spec.tags.contains(tag) {
            Section::Body
        } else if spec::HEADER.tags.contains(tag) {
            Section::Header
        } else if spec::TRAILER.tags.contains(tag) {
            Section::Trailer
        } else if spec::field(tag).is_some() {
            return Err(reject(tag, SessionRejectReason::TagNotDefinedForThisMessageType, "Tag not defined for this message type"))
        } else {
            return Err(reject(tag, SessionRejectReason::InvalidTagNumber, "Unknown tag"))
        };
        if here < section {
            return Err(reject(tag, SessionRejectReason::TagSpecifiedOutOfRequiredOrder, "Tag specified out of required order"))
        }
        section = here;

        let groups = match section {
            Section::Header => spec::HEADER.groups,
            Section::Body => spec.groups,
            Section::Trailer => spec::TRAILER.groups,
        };
        if let Some(group) = groups.iter().find(|g| g.count_tag == tag) {
            if msg.fields.position(tag) != Some(i) {
                return Err(reject(tag, SessionRejectReason::TagAppearsMoreThanOnce, "Tag appears more than once"))
            }
            check_value(msg.raw, &tape[i]).map_err(|(reason, text)| reject(tag, reason, text))?;
            let g = msg.group(group).map_err(|e| reject(e.ref_tag(), e.reject_reason(), "Invalid repeating group"))?
                .expect("count tag is on the tape");
            for entry in g.entries() {
                check_entry(msg.raw, &entry, group).map_err(|(t, reason, text)| reject(t, reason, text))?;
            }
            i += 1 + g.fields().len();
            continue
        }

        if msg.fields.position(tag) != Some(i) {
            return Err(reject(tag, SessionRejectReason::TagAppearsMoreThanOnce, "Tag appears more than once"))
        }
        check_value(msg.raw, &tape[i]).map_err(|(reason, text)| reject(tag, reason, text))?;
        i += 1;
    }

    for &tag in spec.required {
        if msg.fields.position(tag).is_none() {
            return Err(reject(tag, SessionRejectReason::RequiredTagMissing, "Required tag missing"))
        }
    }

    Ok(spec)
}

type EntryError = (u16, SessionRejectReason, &'static str);

/// One group entry: values, repeats, required tags, nested groups.
fn check_entry(raw: &[u8], entry: &Entry, group: &GroupSpec) -> Result<(), EntryError> {
    let fields = entry.fields();
    let mut i = 0;
    while i < fields.len() {
        let tag = fields[i].tag;
        if let Some(nested) = group.groups.iter().find(|g| g.count_tag == tag) {
            check_value(raw, &fields[i]).map_err(|(r, t)| (tag, r, t))?;
            let g = entry.group(nested).map_err(|e| (e.ref_tag(), e.reject_reason(), "Invalid repeating group"))?
                .expect("count tag is in the entry");
            for inner in g.entries() {
                check_entry(raw, &inner, nested)?;
            }
            i += 1 + g.fields().len();
            continue
        }
        // Entries are short: a linear repeat check is fine.
        if fields[..i].iter().any(|f| f.tag == tag) {
            return Err((tag, SessionRejectReason::TagAppearsMoreThanOnce, "Tag appears more than once in a group entry"))
        }
        check_value(raw, &fields[i]).map_err(|(r, t)| (tag, r, t))?;
        i += 1;
    }
    for &tag in group.required {
        if entry.get_slice(tag).is_none() {
            return Err((tag, SessionRejectReason::RequiredTagMissing, "Required group field missing"))
        }
    }
    Ok(())
}

/// Format and enum check for one field.
fn check_value(raw: &[u8], field: &super::flat_index::Field) -> Result<(), (SessionRejectReason, &'static str)> {
    let Some(def) = spec::field(field.tag) else { return Ok(()) };
    let start = field.offset as usize;
    let v = &raw[start..start + field.length as usize];

    let well_formed = match def.ty {
        FieldType::Int => is_int(v),
        FieldType::Length | FieldType::SeqNum | FieldType::NumInGroup => v.iter().all(u8::is_ascii_digit),
        FieldType::Boolean => v == b"Y" || v == b"N",
        FieldType::Char => v.len() == 1,
        FieldType::UtcTimestamp => parse_utc_timestamp(v).is_some(),
        FieldType::Float | FieldType::Price | FieldType::Qty | FieldType::Amt
            | FieldType::Percentage | FieldType::PriceOffset => is_decimal(v),
        _ => true,
    };
    if !well_formed {
        return Err((SessionRejectReason::IncorrectDataFormatForValue, "Incorrect data format for value"))
    }
    if !def.values.is_empty() && !def.values.contains(&v) {
        return Err((SessionRejectReason::ValueIsIncorrect, "Value is incorrect (out of range) for this tag"))
    }
    Ok(())
}

fn is_int(v: &[u8]) -> bool {
    let digits = v.strip_prefix(b"-").unwrap_or(v);
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

fn is_decimal(v: &[u8]) -> bool {
    let body = v.strip_prefix(b"-").unwrap_or(v);
    let mut dots = 0;
    let mut digits = 0;
    for &b in body {
        match b {
            b'0'..=b'9' => digits += 1,
            b'.' => dots += 1,
            _ => return false,
        }
    }
    digits > 0 && dots <= 1
}

/// UTCTimestamp "YYYYMMDD-HH:MM:SS[.fff…]" (up to 9 fraction digits) to
/// nanoseconds since the Unix epoch.
pub fn parse_utc_timestamp(v: &[u8]) -> Option<i64> {
    if v.len() < 17 || v[8] != b'-' || v[11] != b':' || v[14] != b':' {
        return None
    }
    let num = |a: usize, b: usize| -> Option<i64> {
        v[a..b].iter().try_fold(0i64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as i64))
    };
    let (year, month, day) = (num(0, 4)?, num(4, 6)?, num(6, 8)?);
    let (hour, minute, second) = (num(9, 11)?, num(12, 14)?, num(15, 17)?);
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 60 {
        return None
    }

    let mut nanos = 0;
    if v.len() > 17 {
        let frac = &v[17..];
        if frac[0] != b'.' || frac.len() < 2 || frac.len() > 10 {
            return None
        }
        let digits = &frac[1..];
        let n = digits.iter().try_fold(0i64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as i64))?;
        nanos = n * 10i64.pow(9 - digits.len() as u32);
    }

    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000_000_000 + nanos)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{ParseOutcome, Parser};

    const SECOND: i64 = 1_000_000_000;
    // 2026-10-25 17:00:00 UTC
    const NOW: i64 = 1_792_947_600 * SECOND;
    const SENDING_TIME: &str = "20261025-17:00:00.000";

    const RULES: HeaderRules = HeaderRules {
        our_comp_id: b"GATEWAY",
        their_comp_id: b"CLIENT",
        max_sending_time_skew_ns: 120 * SECOND,
    };

    /// Frame with a standard header (49=CLIENT, 56=GATEWAY, 34=`seq`, 52) and
    /// `fields` ('|' for SOH) after it.
    fn frame(msg_type: &str, extra_header: &str, fields: &str) -> Vec<u8> {
        let body = format!("35={msg_type}|49=CLIENT|56=GATEWAY|34=7|52={SENDING_TIME}|{extra_header}{fields}").replace('|', "\x01");
        raw_frame(&body)
    }

    fn raw_frame(body: &str) -> Vec<u8> {
        let body = body.replace('|', "\x01");
        let mut out = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = out.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        out.extend(format!("10={:03}\x01", sum).bytes());
        out
    }

    fn with_message<R>(raw: &[u8], check: impl FnOnce(Message) -> R) -> R {
        let mut parser = Box::new(Parser::new());
        match parser.parse_frame(raw) {
            ParseOutcome::Message(msg) => check(msg),
            _ => panic!("frame didn't parse: {:?}", String::from_utf8_lossy(raw)),
        }
    }

    fn header_err(raw: &[u8]) -> HeaderError<'static> {
        with_message(raw, |msg| {
            let err = validate_header(&msg, &RULES, NOW).expect_err("header should fail");
            // Detach from the buffer for easy matching.
            let detach = |r: SessionReject| SessionReject { ref_msg_type: None, ..r };
            match err {
                HeaderError::Reject(r) => HeaderError::Reject(detach(r)),
                HeaderError::RejectAndLogout(r) => HeaderError::RejectAndLogout(detach(r)),
                HeaderError::Logout { text } => HeaderError::Logout { text },
            }
        })
    }

    fn body_err(raw: &[u8]) -> (Option<u16>, SessionRejectReason) {
        with_message(raw, |msg| {
            let header = validate_header(&msg, &RULES, NOW).expect("header ok");
            let r = validate_body(&msg, &header).expect_err("body should fail");
            assert_eq!(r.ref_seq_num, Some(7));
            (r.ref_tag, r.reason)
        })
    }

    fn body_ok(raw: &[u8]) -> &'static str {
        with_message(raw, |msg| {
            let header = validate_header(&msg, &RULES, NOW).expect("header ok");
            validate_body(&msg, &header).expect("body ok").name
        })
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_utc_timestamp(b"19700101-00:00:00"), Some(0));
        assert_eq!(parse_utc_timestamp(SENDING_TIME.as_bytes()), Some(NOW));
        assert_eq!(parse_utc_timestamp(b"20261025-17:00:00.5"), Some(NOW + SECOND / 2));
        assert_eq!(parse_utc_timestamp(b"20261025-17:00:00.000000001"), Some(NOW + 1));
        assert_eq!(parse_utc_timestamp(b"20240229-00:00:00"), Some(1_709_164_800 * SECOND));
        for bad in [&b"20230229-00:00:00"[..], b"20261325-17:00:00", b"20261025-24:00:00", b"20261025 17:00:00",
                    b"20261025-17:00:00.", b"20261025-17:00:00.1234567890", b"2026102-17:00:00", b"20261025-17:0a:00"] {
            assert_eq!(parse_utc_timestamp(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn valid_header() {
        with_message(&frame("0", "43=Y|122=20261025-16:59:59|", ""), |msg| {
            let h = validate_header(&msg, &RULES, NOW).unwrap();
            assert_eq!(h.msg_type, b"0");
            assert_eq!((h.sender_comp_id, h.target_comp_id), (&b"CLIENT"[..], &b"GATEWAY"[..]));
            assert_eq!((h.msg_seq_num, h.sending_time), (7, NOW));
            assert!(h.poss_dup && !h.poss_resend);
            assert_eq!(h.orig_sending_time, Some(NOW - SECOND));
        });
    }

    #[test]
    fn missing_msg_seq_num_is_logout() {
        let raw = raw_frame(&format!("35=0|49=CLIENT|56=GATEWAY|52={SENDING_TIME}|"));
        assert!(matches!(header_err(&raw), HeaderError::Logout { .. }));
        let raw = raw_frame(&format!("35=0|49=CLIENT|56=GATEWAY|34=x|52={SENDING_TIME}|"));
        assert!(matches!(header_err(&raw), HeaderError::Logout { .. }));
    }

    #[test]
    fn missing_required_header_field() {
        let raw = raw_frame("35=0|49=CLIENT|56=GATEWAY|34=7|");
        match header_err(&raw) {
            HeaderError::Reject(r) => assert_eq!((r.ref_tag, r.reason), (Some(52), SessionRejectReason::RequiredTagMissing)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn comp_id_mismatch_rejects_and_logs_out() {
        let raw = raw_frame(&format!("35=0|49=SOMEONE|56=GATEWAY|34=7|52={SENDING_TIME}|"));
        match header_err(&raw) {
            HeaderError::RejectAndLogout(r) => assert_eq!((r.ref_tag, r.reason), (Some(49), SessionRejectReason::CompIdProblem)),
            other => panic!("{other:?}"),
        }
        let raw = raw_frame(&format!("35=0|49=CLIENT|56=OTHER|34=7|52={SENDING_TIME}|"));
        match header_err(&raw) {
            HeaderError::RejectAndLogout(r) => assert_eq!(r.ref_tag, Some(56)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sending_time_checks() {
        let raw = raw_frame("35=0|49=CLIENT|56=GATEWAY|34=7|52=20261025-16:50:00|");
        match header_err(&raw) {
            HeaderError::RejectAndLogout(r) => assert_eq!((r.ref_tag, r.reason), (Some(52), SessionRejectReason::SendingTimeAccuracyProblem)),
            other => panic!("{other:?}"),
        }
        let raw = raw_frame("35=0|49=CLIENT|56=GATEWAY|34=7|52=yesterday-ish....|");
        match header_err(&raw) {
            HeaderError::Reject(r) => assert_eq!((r.ref_tag, r.reason), (Some(52), SessionRejectReason::IncorrectDataFormatForValue)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn poss_dup_rules() {
        match header_err(&frame("0", "43=Y|", "")) {
            HeaderError::Reject(r) => assert_eq!((r.ref_tag, r.reason), (Some(122), SessionRejectReason::RequiredTagMissing)),
            other => panic!("{other:?}"),
        }
        match header_err(&frame("0", "43=Y|122=20261025-17:00:01|", "")) {
            HeaderError::RejectAndLogout(r) => assert_eq!((r.ref_tag, r.reason), (Some(122), SessionRejectReason::SendingTimeAccuracyProblem)),
            other => panic!("{other:?}"),
        }
        match header_err(&frame("0", "43=maybe|", "")) {
            HeaderError::Reject(r) => assert_eq!((r.ref_tag, r.reason), (Some(43), SessionRejectReason::IncorrectDataFormatForValue)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn session_messages_pass() {
        assert_eq!(body_ok(&frame("0", "", "")), "Heartbeat");
        assert_eq!(body_ok(&frame("0", "", "112=ping|")), "Heartbeat");
        assert_eq!(body_ok(&frame("1", "", "112=ping|")), "TestRequest");
        assert_eq!(body_ok(&frame("2", "", "7=1|16=0|")), "ResendRequest");
        assert_eq!(body_ok(&frame("3", "", "45=3|371=55|372=D|373=5|58=bad|")), "Reject");
        assert_eq!(body_ok(&frame("4", "", "123=Y|36=10|")), "SequenceReset");
        assert_eq!(body_ok(&frame("5", "", "58=bye|")), "Logout");
        assert_eq!(body_ok(&frame("A", "", "98=0|108=30|141=Y|384=2|372=D|385=S|372=8|385=R|553=user|")), "Logon");
        assert_eq!(body_ok(&frame("D", "", "11=a|453=1|448=P|452=3|55=MSFT|54=1|60=20261025-17:00:00|38=10|40=1|")), "NewOrderSingle");
        assert_eq!(body_ok(&frame("j", "", "45=3|372=D|380=3|")), "BusinessMessageReject");
        // Header group and optional header fields.
        assert_eq!(body_ok(&frame("0", "50=desk|627=2|628=HOP1|629=20261025-16:59:59|628=HOP2|", "")), "Heartbeat");
    }

    #[test]
    fn unknown_msg_type() {
        // MarketDataRequest isn't in the dictionary yet.
        assert_eq!(body_err(&frame("V", "", "55=MSFT|")), (Some(35), SessionRejectReason::InvalidMsgType));
    }

    #[test]
    fn field_not_in_message() {
        // NoTradingSessions in a Heartbeat: in the dictionary? Not yet, so unknown tag.
        assert_eq!(body_err(&frame("0", "", "386=1|")), (Some(386), SessionRejectReason::InvalidTagNumber));
        // Known field, wrong message.
        assert_eq!(body_err(&frame("0", "", "108=30|")), (Some(108), SessionRejectReason::TagNotDefinedForThisMessageType));
        // Group member outside its group.
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|385=S|")), (Some(385), SessionRejectReason::TagNotDefinedForThisMessageType));
    }

    #[test]
    fn order_and_repeats() {
        // Header field after the body started.
        assert_eq!(body_err(&frame("1", "", "112=ping|50=desk|")), (Some(50), SessionRejectReason::TagSpecifiedOutOfRequiredOrder));
        // Repeat outside a group.
        assert_eq!(body_err(&frame("1", "", "112=a|112=b|")), (Some(112), SessionRejectReason::TagAppearsMoreThanOnce));
        // Repeat inside one entry.
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|384=1|372=D|385=S|385=R|")), (Some(385), SessionRejectReason::TagAppearsMoreThanOnce));
    }

    #[test]
    fn groups() {
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|384=2|372=D|385=S|")),
            (Some(384), SessionRejectReason::IncorrectNumInGroupCountForRepeatingGroup));
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|384=1|385=S|372=D|")),
            (Some(385), SessionRejectReason::RepeatingGroupFieldsOutOfOrder));
        assert_eq!(body_err(&frame("0", "627=1|628=H|629=bad|", "")),
            (Some(629), SessionRejectReason::IncorrectDataFormatForValue));
    }

    #[test]
    fn required_and_values() {
        assert_eq!(body_err(&frame("1", "", "")), (Some(112), SessionRejectReason::RequiredTagMissing));
        assert_eq!(body_err(&frame("A", "", "98=0|")), (Some(108), SessionRejectReason::RequiredTagMissing));
        assert_eq!(body_err(&frame("A", "", "98=9|108=30|")), (Some(98), SessionRejectReason::ValueIsIncorrect));
        assert_eq!(body_err(&frame("A", "", "98=0|108=thirty|")), (Some(108), SessionRejectReason::IncorrectDataFormatForValue));
        assert_eq!(body_err(&frame("4", "", "123=X|36=10|")), (Some(123), SessionRejectReason::IncorrectDataFormatForValue));
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|384=1|372=D|385=SR|")), (Some(385), SessionRejectReason::IncorrectDataFormatForValue));
        assert_eq!(body_err(&frame("A", "", "98=0|108=30|384=1|372=D|385=X|")), (Some(385), SessionRejectReason::ValueIsIncorrect));
    }
}
