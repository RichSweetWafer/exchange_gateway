//! Writes outbound FIX messages into a caller-owned buffer, without allocating.
//!
//! The body (35= onwards) is written at a fixed offset. `finish` then writes
//! "8=FIX.4.4|9=<len>|" right in front of it and the "10=xxx|" trailer after
//! it, so BodyLength needs no padding and nothing gets moved.

use super::FixVersion;
use super::decode::Decimal;
use super::spec::tags;

const SOH: u8 = 0x01;
/// Room for "8=FIX.x.y|9=" + up to 7 BodyLength digits + SOH.
const PREFIX_ROOM: usize = 32;
/// "10=" + 3 digits + SOH.
const TRAILER_LEN: usize = 7;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EncodeError {
    /// The message doesn't fit the buffer.
    BufferFull,
    /// Empty value, or a value containing SOH.
    InvalidValue { tag: u16 },
}

/// Builds one message. Fields are written in call order.
pub struct MessageWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
    version: FixVersion,
}

impl<'a> MessageWriter<'a> {
    /// Starts a message of type `msg_type` (35).
    pub fn new(buf: &'a mut [u8], version: FixVersion, msg_type: &[u8]) -> Result<Self, EncodeError> {
        if buf.len() < PREFIX_ROOM + TRAILER_LEN {
            return Err(EncodeError::BufferFull)
        }
        let mut w = Self { buf, pos: PREFIX_ROOM, version };
        w.bytes(tags::MSG_TYPE, msg_type)?;
        Ok(w)
    }

    /// Writes the StandardHeader fields the session owns: 49, 56, 34, 52.
    pub fn header(&mut self, sender_comp_id: &[u8], target_comp_id: &[u8], msg_seq_num: u32, sending_time_ns: i64) -> Result<&mut Self, EncodeError> {
        self.bytes(tags::SENDER_COMP_ID, sender_comp_id)?
            .bytes(tags::TARGET_COMP_ID, target_comp_id)?
            .u64(tags::MSG_SEQ_NUM, msg_seq_num as u64)?
            .timestamp(tags::SENDING_TIME, sending_time_ns)
    }

    pub fn bytes(&mut self, tag: u16, value: &[u8]) -> Result<&mut Self, EncodeError> {
        if value.is_empty() || value.contains(&SOH) {
            return Err(EncodeError::InvalidValue { tag })
        }
        self.tag(tag)?;
        self.raw(value)?;
        self.raw(&[SOH])?;
        Ok(self)
    }

    pub fn str(&mut self, tag: u16, value: &str) -> Result<&mut Self, EncodeError> {
        self.bytes(tag, value.as_bytes())
    }

    pub fn u64(&mut self, tag: u16, value: u64) -> Result<&mut Self, EncodeError> {
        let mut digits = [0u8; 20];
        let n = format_u64(value, &mut digits);
        self.bytes(tag, &digits[20 - n..])
    }

    pub fn i64(&mut self, tag: u16, value: i64) -> Result<&mut Self, EncodeError> {
        let mut digits = [0u8; 21];
        let n = format_u64(value.unsigned_abs(), (&mut digits[1..]).try_into().unwrap());
        let start = 21 - n - (value < 0) as usize;
        if value < 0 {
            digits[start] = b'-';
        }
        self.bytes(tag, &digits[start..])
    }

    /// `mantissa / 10^scale` with exactly `scale` decimals: (150, 2) is "1.50".
    pub fn decimal(&mut self, tag: u16, value: Decimal) -> Result<&mut Self, EncodeError> {
        // 20 digits, a dot, a sign, and leading zeros up to 19 decimals.
        let mut text = [0u8; 42];
        let scale = value.scale as usize;
        if scale > 19 {
            return Err(EncodeError::InvalidValue { tag })
        }
        let mut digits = [0u8; 20];
        let n = format_u64(value.mantissa.unsigned_abs(), &mut digits);
        let int_len = n.saturating_sub(scale).max(1);
        let frac_len = scale;
        let mut p = 0;
        if value.mantissa < 0 {
            text[p] = b'-';
            p += 1;
        }
        // Left-pad with zeros so there's at least one integer digit and `scale` decimals.
        let padded = int_len + frac_len;
        let zeros = padded - n;
        for k in 0..padded {
            let d = if k < zeros { b'0' } else { digits[20 - n + k - zeros] };
            if k == int_len {
                text[p] = b'.';
                p += 1;
            }
            text[p] = d;
            p += 1;
        }
        self.bytes(tag, &text[..p])
    }

    pub fn bool(&mut self, tag: u16, value: bool) -> Result<&mut Self, EncodeError> {
        self.bytes(tag, if value { b"Y" } else { b"N" })
    }

    /// UTCTimestamp with milliseconds, e.g. 20261025-17:00:00.123.
    pub fn timestamp(&mut self, tag: u16, ns_since_epoch: i64) -> Result<&mut Self, EncodeError> {
        let mut text = [0u8; 21];
        format_utc_timestamp(ns_since_epoch, &mut text);
        self.bytes(tag, &text)
    }

    /// Writes BeginString, BodyLength and CheckSum and returns the whole message.
    pub fn finish(self) -> Result<&'a [u8], EncodeError> {
        let body_len = self.pos - PREFIX_ROOM;

        let mut len_digits = [0u8; 20];
        let n = format_u64(body_len as u64, &mut len_digits);
        let version = self.version.as_bytes();
        let prefix_len = b"8=FIX.".len() + version.len() + 1 + b"9=".len() + n + 1;
        if prefix_len > PREFIX_ROOM {
            return Err(EncodeError::BufferFull)
        }
        let start = PREFIX_ROOM - prefix_len;
        let mut p = start;
        for part in [&b"8=FIX."[..], version, &[SOH], b"9=", &len_digits[20 - n..], &[SOH]] {
            self.buf[p..p + part.len()].copy_from_slice(part);
            p += part.len();
        }

        let end = self.pos;
        if end + TRAILER_LEN > self.buf.len() {
            return Err(EncodeError::BufferFull)
        }
        let sum = self.buf[start..end].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        let trailer = [b'1', b'0', b'=', b'0' + sum / 100, b'0' + sum / 10 % 10, b'0' + sum % 10, SOH];
        self.buf[end..end + TRAILER_LEN].copy_from_slice(&trailer);

        Ok(&self.buf[start..end + TRAILER_LEN])
    }

    fn tag(&mut self, tag: u16) -> Result<(), EncodeError> {
        let mut digits = [0u8; 20];
        let n = format_u64(tag as u64, &mut digits);
        self.raw(&digits[20 - n..])?;
        self.raw(b"=")
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        // Keep room for the trailer.
        let end = self.pos + bytes.len();
        if end + TRAILER_LEN > self.buf.len() {
            return Err(EncodeError::BufferFull)
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }
}

/// Writes `value` right-aligned into `out` and returns the digit count.
fn format_u64(mut value: u64, out: &mut [u8; 20]) -> usize {
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return out.len() - i
        }
    }
}

/// "YYYYMMDD-HH:MM:SS.sss" into `out`.
pub fn format_utc_timestamp(ns_since_epoch: i64, out: &mut [u8; 21]) {
    let ms = ns_since_epoch.div_euclid(1_000_000);
    let secs = ms.div_euclid(1000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let put = |out: &mut [u8; 21], at: usize, value: i64, width: usize| {
        let mut v = value;
        for k in (0..width).rev() {
            out[at + k] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    put(out, 0, year, 4);
    put(out, 4, month, 2);
    put(out, 6, day, 2);
    out[8] = b'-';
    put(out, 9, sod / 3600, 2);
    out[11] = b':';
    put(out, 12, sod / 60 % 60, 2);
    out[14] = b':';
    put(out, 15, sod % 60, 2);
    out[17] = b'.';
    put(out, 18, ms.rem_euclid(1000), 3);
}

/// Inverse of days_from_civil (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    (year, month, day)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{ParseOutcome, Parser};
    use crate::fix::spec::MsgType;
    use crate::fix::validate::{HeaderRules, parse_utc_timestamp, validate_header};
    use crate::fix::views::{self, SessionMessage};

    const SECOND: i64 = 1_000_000_000;
    const NOW: i64 = 1_792_947_600 * SECOND + 123_456_789; // 2026-10-25 17:00:00.123456789 UTC

    #[test]
    fn timestamps_round_trip() {
        let mut text = [0u8; 21];
        format_utc_timestamp(NOW, &mut text);
        assert_eq!(&text, b"20261025-17:00:00.123");
        assert_eq!(parse_utc_timestamp(&text), Some(NOW - 456_789));

        format_utc_timestamp(0, &mut text);
        assert_eq!(&text, b"19700101-00:00:00.000");
        format_utc_timestamp(1_709_164_800 * SECOND, &mut text);
        assert_eq!(&text, b"20240229-00:00:00.000");
        for days in [-1000i64, 0, 59, 365, 10_957, 20_000, 60_000] {
            let ns = days * 86_400 * SECOND + 3_723_004 * 1_000_000;
            format_utc_timestamp(ns, &mut text);
            assert_eq!(parse_utc_timestamp(&text), Some(ns), "{days}");
        }
    }

    #[test]
    fn heartbeat_bytes() {
        let mut buf = [0u8; 256];
        let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, MsgType::Heartbeat.as_bytes()).unwrap();
        w.header(b"GATEWAY", b"CLIENT", 1, 1_792_947_600 * SECOND).unwrap();
        let msg = w.finish().unwrap();
        let body = b"35=0\x0149=GATEWAY\x0156=CLIENT\x0134=1\x0152=20261025-17:00:00.000\x01";
        let mut want = format!("8=FIX.4.4\x019={}\x01", body.len()).into_bytes();
        want.extend_from_slice(body);
        let sum = want.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        want.extend(format!("10={sum:03}\x01").bytes());
        assert_eq!(msg, &want[..]);
    }

    #[test]
    fn logon_round_trips_through_parser_and_views() {
        let mut buf = [0u8; 512];
        let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, MsgType::Logon.as_bytes()).unwrap();
        w.header(b"CLIENT", b"GATEWAY", 42, NOW).unwrap()
            .u64(tags::ENCRYPT_METHOD, 0).unwrap()
            .u64(tags::HEART_BT_INT, 30).unwrap()
            .bool(tags::RESET_SEQ_NUM_FLAG, true).unwrap()
            .str(tags::USERNAME, "user").unwrap();
        let raw = w.finish().unwrap().to_vec();

        let mut parser = Box::new(Parser::new());
        let ParseOutcome::Message(msg) = parser.parse_frame(&raw) else { panic!("didn't parse") };
        let body_start = raw.windows(4).position(|w| w == b"\x0135=").unwrap() + 1;
        assert_eq!(msg.get_int(9), Some(Ok((raw.len() - TRAILER_LEN - body_start) as i64)));
        let rules = HeaderRules { our_comp_id: b"GATEWAY", their_comp_id: b"CLIENT", max_sending_time_skew_ns: SECOND };
        let header = validate_header(&msg, &rules, NOW).unwrap();
        assert_eq!(header.msg_seq_num, 42);
        let SessionMessage::Logon(logon) = views::decode(&msg, &header).unwrap() else { panic!() };
        assert_eq!((logon.heart_bt_int, logon.reset_seq_num, logon.username), (30, true, Some("user")));
    }

    #[test]
    fn numbers() {
        let mut buf = [0u8; 128];
        let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, b"0").unwrap();
        w.u64(1, 0).unwrap().u64(2, u64::MAX).unwrap().i64(3, -42).unwrap().i64(4, i64::MIN).unwrap();
        let msg = w.finish().unwrap();
        let text = String::from_utf8_lossy(msg).replace('\x01', "|");
        assert!(text.contains("|1=0|2=18446744073709551615|3=-42|4=-9223372036854775808|"), "{text}");
    }

    #[test]
    fn decimals() {
        let mut buf = [0u8; 256];
        let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, b"0").unwrap();
        for (tag, (mantissa, scale)) in [(150, 2), (5, 2), (-12345, 3), (7, 0), (0, 0), (0, 3), (i64::MIN, 4)].into_iter().enumerate() {
            w.decimal(tag as u16 + 1, Decimal::new(mantissa, scale)).unwrap();
        }
        let msg = w.finish().unwrap();
        let text = String::from_utf8_lossy(msg).replace('\x01', "|");
        assert!(text.contains("|1=1.50|2=0.05|3=-12.345|4=7|5=0|6=0.000|7=-922337203685477.5808|"), "{text}");
        for v in ["1.50", "0.05", "-12.345", "7"] {
            let mut buf = [0u8; 64];
            let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, b"0").unwrap();
            w.decimal(44, crate::fix::decode::to_decimal(v.as_bytes()).unwrap()).unwrap();
            assert!(String::from_utf8_lossy(w.finish().unwrap()).contains(&format!("\x0144={v}\x01")), "{v}");
        }
    }

    #[test]
    fn errors() {
        let mut small = [0u8; 50];
        let mut w = MessageWriter::new(&mut small, FixVersion::Fix44, b"0").unwrap();
        assert_eq!(w.str(58, "this text does not fit").err(), Some(EncodeError::BufferFull));

        let mut buf = [0u8; 128];
        let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, b"0").unwrap();
        assert_eq!(w.str(58, "").err(), Some(EncodeError::InvalidValue { tag: 58 }));
        assert_eq!(w.bytes(58, b"a\x01b").err(), Some(EncodeError::InvalidValue { tag: 58 }));
        assert!(MessageWriter::new(&mut [0u8; 10], FixVersion::Fix44, b"0").is_err());
    }
}
