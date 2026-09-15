//! EXPERIMENTAL: `parser_v2` plus three alternative field scanners, chosen by `Mode`:
//! - `Memchr`: SWAR '=' search and tag decode, memchr for each value's SOH.
//! - `Sse2` / `Avx2`: simdjson-style structural index. SOH, '=' and "bad byte"
//!   bitmasks per 16/32 bytes, walked with trailing_zeros. AVX2 is detected at
//!   runtime and falls back to SSE2.
//! Fast paths only handle clean input; anything unusual falls back to the scalar
//! `parse_fields`, so Rejects are identical. Fields go into `FlatIndexMap`.
//!
//! Verified: the 19 parser tests in every mode, exhaustive `decode_tag` check,
//! 200k-frame differential test (`diff_tests`).
//! Measured (best ns/msg vs scalar): ORDER 165 -> memchr 229 / sse2 185 / avx2 168;
//! 64 varying frames 230 -> 316 / 251 / 218; 124-field frame 1419 -> 1810 / 1459 / 1268.
//! So SIMD alone gives at most ~1.1x on this CPU.

#[cfg(debug_assertions)]
use tracing::trace;

use crate::fix::Message;
use crate::fix::flat_index_map::FlatIndexMap;

use crate::fix::decode;

const FIX_SEP: u8 = b'\x01';
const FIX_TAG_VAL_SEP: u8 = b'=';
const FIX_CHECKSUM_TAG: u16 = 10;
/// "10=" + 3 digits + SOH
const TRAILER_LEN: usize = 7;
/// u16 tags never need more than 5 digits.
const MAX_TAG_DIGITS: usize = 5;
/// BeginString, BodyLength and MsgType must be the first three fields.
const HEADER_TAGS: [u16; 3] = [8, 9, 35];


/// Why a frame is dropped silently. No Reject is sent and the inbound
/// sequence number is not advanced.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GarbledKind {
    /// Frame doesn't end in "<SOH>10=xxx<SOH>".
    MalformedTrailer,
    /// CheckSum isn't a 3 digit value in 0..=255.
    MalformedChecksum,
    ChecksumMismatch { expected: u8, actual: u8 },
}

/// SessionRejectReason (373) values the parser can detect.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
pub enum RejectReason {
    InvalidTag = 0,
    RequiredTagMissing = 1,
    TagWithoutValue = 4,
    IncorrectDataFormat = 6,
    TagRepeated = 13,
    TagOutOfOrder = 14,
}

pub enum ParseOutcome<'a> {
    Message(Message<'a>),
    Garbled(GarbledKind),
    /// The checksum is good but a field isn't. `partial` holds the fields
    /// before the bad one, which is enough to find MsgSeqNum (34) for RefSeqNum.
    /// `tag` is 0 when the tag itself couldn't be read.
    Reject { partial: Message<'a>, tag: u16, reason: RejectReason },
}

struct FieldError {
    tag: u16,
    reason: RejectReason,
}

impl FieldError {
    #[inline(always)]
    fn new(tag: u16, reason: RejectReason) -> Self {
        Self { tag, reason }
    }
}

#[derive(Debug,Clone, Copy)]
pub struct Parser {
    fields: FlatIndexMap,
    mode: Mode,
    avx2: bool,
}


impl Parser {

    pub fn new() -> Self {
        Self::with_mode(Mode::Scalar)
    }

    /// Parses one frame from the framer, from "8=" through the checksum's SOH.
    /// Field offsets are relative to the start of `frame`.
    pub fn parse_frame<'a>(&'a mut self, frame: &'a [u8]) -> ParseOutcome<'a> {
        self.fields.clear();

        // Body includes the SOH before "10=". Ending in SOH also keeps the
        // field loops inside the body without checking for its end.
        let body_len = match frame.len().checked_sub(TRAILER_LEN) {
            Some(len) if len > 0 => len,
            _ => return ParseOutcome::Garbled(GarbledKind::MalformedTrailer),
        };
        let (body, trailer) = frame.split_at(body_len);
        if body[body_len - 1] != FIX_SEP || &trailer[..3] != b"10=" || trailer[6] != FIX_SEP {
            return ParseOutcome::Garbled(GarbledKind::MalformedTrailer)
        }
        let expected = match decode::to_u32(&trailer[3..6]) {
            Some(value) if value <= u8::MAX as u32 => value as u8,
            _ => return ParseOutcome::Garbled(GarbledKind::MalformedChecksum),
        };

        if self.mode != Mode::Scalar {
            return self.parse_fast(frame, body_len, expected)
        }

        let result = self.parse_fields(body);

        // A bad field stops the inline sum early, so recount. Garbled wins
        // over Reject: a corrupted message must not be answered.
        let sum = match result {
            Ok(sum) => sum,
            Err(_) => body.iter().fold(0u32, |sum, &b| sum.wrapping_add(b as u32)),
        };
        let actual = sum as u8;
        if actual != expected {
            #[cfg(debug_assertions)]
            trace!(expected, actual, "checksum mismatch");
            return ParseOutcome::Garbled(GarbledKind::ChecksumMismatch { expected, actual })
        }

        match result {
            Ok(_) => {
                // Can't be a duplicate: 10 inside the body is rejected.
                let _ = self.fields.insert(FIX_CHECKSUM_TAG, (body_len + 3) as u32, 3);
                ParseOutcome::Message(Message { raw: frame, fields: &self.fields })
            }
            Err(e) => {
                #[cfg(debug_assertions)]
                trace!(tag = e.tag, reason = ?e.reason, "field rejected");
                ParseOutcome::Reject {
                    partial: Message { raw: frame, fields: &self.fields },
                    tag: e.tag,
                    reason: e.reason,
                }
            }
        }
    }

    /// Indexes every field in `body`, which must end in SOH, and returns the
    /// byte sum for the checksum.
    #[inline(always)]
    fn parse_fields(&mut self, body: &[u8]) -> Result<u32, FieldError> {
        let mut sum = 0u32;
        let mut i = 0;
        let mut field_idx = 0;

        while i < body.len() {
            // Tag: digits up to '='. The body's final SOH is a non-digit, so
            // this stops inside the body.
            let tag_start = i;
            let mut tag = 0u32;
            loop {
                let b = body[i];
                sum = sum.wrapping_add(b as u32);
                i += 1;

                let digit = b.wrapping_sub(b'0');
                if digit > 9 {
                    if b != FIX_TAG_VAL_SEP {
                        return Err(FieldError::new(0, RejectReason::InvalidTag))
                    }
                    break
                }
                // Wraps on absurd lengths, which the digit count below rejects.
                tag = tag.wrapping_mul(10).wrapping_add(digit as u32);
            }

            let digits = i - 1 - tag_start;
            if digits == 0 || digits > MAX_TAG_DIGITS || tag == 0 || tag > u16::MAX as u32 {
                return Err(FieldError::new(0, RejectReason::InvalidTag))
            }
            let tag = tag as u16;

            // Value: printable ASCII up to SOH.
            let value_start = i;
            loop {
                let b = body[i];
                sum = sum.wrapping_add(b as u32);
                i += 1;

                // One compare covers 0x20..=0x7E. SOH and everything else
                // take the slow path.
                if b.wrapping_sub(0x20) > 0x7E - 0x20 {
                    if b != FIX_SEP {
                        return Err(FieldError::new(tag, RejectReason::IncorrectDataFormat))
                    }
                    break
                }
            }

            let length = i - 1 - value_start;
            if length == 0 {
                return Err(FieldError::new(tag, RejectReason::TagWithoutValue))
            }
            if (field_idx < HEADER_TAGS.len() && tag != HEADER_TAGS[field_idx]) || tag == FIX_CHECKSUM_TAG {
                return Err(FieldError::new(tag, RejectReason::TagOutOfOrder))
            }

            // TODO: FlatIndexMap is for unique fields only, add storage for repeating fields
            if self.fields.insert(tag, value_start as u32, length as u32).is_err() {
                // Length is non-zero, so the tag is either a repeat or beyond the map.
                let reason = if self.fields.get_slice(tag, body).is_some() {
                    RejectReason::TagRepeated
                } else {
                    RejectReason::InvalidTag
                };
                return Err(FieldError::new(tag, reason))
            }
            field_idx += 1;
        }

        if field_idx < HEADER_TAGS.len() {
            return Err(FieldError::new(HEADER_TAGS[field_idx], RejectReason::RequiredTagMissing))
        }

        Ok(sum)
    }

}





// ---------------------------------------------------------------------------
// Fast scanners (prototype). Each one only handles well-formed input and bails
// out with Err(()) on anything unusual; the scalar parse_fields then produces
// the exact Reject. Bad frames are rare, so the slow path stays off the hot path.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Scalar,
    Memchr,
    Sse2,
    /// Falls back to SSE2 when the CPU has no AVX2.
    Avx2,
}

type Fast = Result<(), ()>;

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// Decodes 1..=5 ASCII digits at frame[start..start + len] without a per-digit loop.
/// Reads 8 bytes, which is safe for any start inside the body: the 7 byte trailer follows it.
#[inline(always)]
pub fn decode_tag(frame: &[u8], start: usize, len: usize) -> Option<u32> {
    if len == 0 || len > MAX_TAG_DIGITS {
        return None
    }
    let x = u64::from_le_bytes(frame.get(start..start + 8)?.try_into().unwrap());
    let keep = (1u64 << (8 * len)) - 1;
    // Unused bytes become '0' so the digit check covers all 8.
    let a = (x & keep) | (0x3030_3030_3030_3030 & !keep);
    if a & 0xF0F0_F0F0_F0F0_F0F0 != 0x3030_3030_3030_3030
        || a.wrapping_add(0x0606_0606_0606_0606) & 0xF0F0_F0F0_F0F0_F0F0 != 0x3030_3030_3030_3030
    {
        return None
    }
    // First digit is the lowest byte. Shifting left turns the padding into leading zeros.
    let mut v = (a & 0x0F0F_0F0F_0F0F_0F0F) << (8 * (8 - len));
    v = v.wrapping_mul(2561) >> 8;
    v = (v & 0x00FF_00FF_00FF_00FF).wrapping_mul(6553601) >> 16;
    v = (v & 0x0000_FFFF_0000_FFFF).wrapping_mul(42949672960001) >> 32;
    Some(v as u32)
}

/// Position of the first '=' in the 8 bytes at frame[start..], if any.
#[inline(always)]
fn find_eq_swar(frame: &[u8], start: usize) -> Option<usize> {
    let x = u64::from_le_bytes(frame.get(start..start + 8)?.try_into().unwrap());
    let y = x ^ (ONES * b'=' as u64);
    let z = y.wrapping_sub(ONES) & !y & HIGHS;
    (z != 0).then(|| (z.trailing_zeros() / 8) as usize)
}

#[cfg(target_arch = "x86_64")]
pub mod simd {
    use std::arch::x86_64::*;

    /// (SOH, '=', bad) bitmasks for 16 bytes. Bad = not printable ASCII and not SOH.
    #[inline(always)]
    pub unsafe fn masks_sse2(p: *const u8) -> (u64, u64, u64) {
        unsafe {
            let v = _mm_loadu_si128(p as *const __m128i);
            let soh = _mm_cmpeq_epi8(v, _mm_set1_epi8(0x01));
            let eq = _mm_cmpeq_epi8(v, _mm_set1_epi8(b'=' as i8));
            // Signed compares: bytes >= 0x80 are negative, so they fail "> 0x1F".
            let printable = _mm_and_si128(
                _mm_cmpgt_epi8(v, _mm_set1_epi8(0x1F)),
                _mm_cmplt_epi8(v, _mm_set1_epi8(0x7F)),
            );
            let ok = _mm_or_si128(printable, soh);
            (
                _mm_movemask_epi8(soh) as u32 as u64,
                _mm_movemask_epi8(eq) as u32 as u64,
                (!(_mm_movemask_epi8(ok) as u32) & 0xFFFF) as u64,
            )
        }
    }

    #[inline(always)]
    pub unsafe fn masks_avx2(p: *const u8) -> (u64, u64, u64) {
        unsafe {
            let v = _mm256_loadu_si256(p as *const __m256i);
            let soh = _mm256_cmpeq_epi8(v, _mm256_set1_epi8(0x01));
            let eq = _mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'=' as i8));
            let printable = _mm256_and_si256(
                _mm256_cmpgt_epi8(v, _mm256_set1_epi8(0x1F)),
                _mm256_cmpgt_epi8(_mm256_set1_epi8(0x7F), v),
            );
            let ok = _mm256_or_si256(printable, soh);
            (
                _mm256_movemask_epi8(soh) as u32 as u64,
                _mm256_movemask_epi8(eq) as u32 as u64,
                (!(_mm256_movemask_epi8(ok) as u32)) as u64,
            )
        }
    }
}

impl Parser {
    pub fn with_mode(mode: Mode) -> Self {
        #[cfg(target_arch = "x86_64")]
        let avx2 = std::is_x86_feature_detected!("avx2");
        #[cfg(not(target_arch = "x86_64"))]
        let avx2 = false;
        Self { fields: FlatIndexMap::new(), mode, avx2 }
    }

    /// Which scanner actually runs for this parser.
    pub fn isa(&self) -> &'static str {
        match self.mode {
            Mode::Scalar => "scalar",
            Mode::Memchr => "memchr",
            Mode::Avx2 if self.avx2 => "avx2",
            _ => if cfg!(target_arch = "x86_64") { "sse2" } else { "memchr (no x86)" },
        }
    }

    #[inline(always)]
    fn parse_fast<'a>(&'a mut self, frame: &'a [u8], body_len: usize, expected: u8) -> ParseOutcome<'a> {
        let body = &frame[..body_len];

        // Separate pass; the compiler vectorizes both folds.
        let (actual, bad) = if self.mode == Mode::Memchr {
            body.iter().fold((0u8, 0u8), |(sum, bad), &b| {
                (sum.wrapping_add(b), bad | ((b != FIX_SEP) & (b.wrapping_sub(0x20) > 0x7E - 0x20)) as u8)
            })
        } else {
            (body.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)), 0)
        };
        if actual != expected {
            return ParseOutcome::Garbled(GarbledKind::ChecksumMismatch { expected, actual })
        }

        let fast = if bad != 0 {
            Err(())
        } else {
            match self.mode {
                Mode::Memchr => self.fields_memchr(frame, body_len),
                #[cfg(target_arch = "x86_64")]
                Mode::Avx2 if self.avx2 => unsafe { self.fields_avx2(frame, body_len) },
                #[cfg(target_arch = "x86_64")]
                _ => self.walk::<16>(frame, body_len, |p| unsafe { simd::masks_sse2(p) }),
                #[cfg(not(target_arch = "x86_64"))]
                _ => self.fields_memchr(frame, body_len),
            }
        };

        if fast.is_err() {
            self.fields.clear();
            if let Err(e) = self.parse_fields(body) {
                return ParseOutcome::Reject {
                    partial: Message { raw: frame, fields: &self.fields },
                    tag: e.tag,
                    reason: e.reason,
                }
            }
        }

        let _ = self.fields.insert(FIX_CHECKSUM_TAG, (body_len + 3) as u32, 3);
        ParseOutcome::Message(Message { raw: frame, fields: &self.fields })
    }

    #[inline(always)]
    fn fast_field(&mut self, field_idx: &mut usize, tag: u32, value_start: usize, length: usize) -> Fast {
        if length == 0 || tag == 0 || tag > u16::MAX as u32 {
            return Err(())
        }
        let tag = tag as u16;
        if (*field_idx < HEADER_TAGS.len() && tag != HEADER_TAGS[*field_idx]) || tag == FIX_CHECKSUM_TAG {
            return Err(())
        }
        self.fields.insert(tag, value_start as u32, length as u32).map_err(|_| ())?;
        *field_idx += 1;
        Ok(())
    }

    /// SWAR '=' search and tag decode, memchr for the value's SOH.
    #[inline(always)]
    fn fields_memchr(&mut self, frame: &[u8], body_len: usize) -> Fast {
        let body = &frame[..body_len];
        let mut i = 0;
        let mut field_idx = 0;
        while i < body_len {
            let tag_len = find_eq_swar(frame, i).ok_or(())?;
            let tag = decode_tag(frame, i, tag_len).ok_or(())?;
            let value_start = i + tag_len + 1;
            let value_end = value_start + memchr::memchr(FIX_SEP, &body[value_start..]).ok_or(())?;
            self.fast_field(&mut field_idx, tag, value_start, value_end - value_start)?;
            i = value_end + 1;
        }
        if field_idx < HEADER_TAGS.len() { Err(()) } else { Ok(()) }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn fields_avx2(&mut self, frame: &[u8], body_len: usize) -> Fast {
        self.walk::<32>(frame, body_len, |p| unsafe { simd::masks_avx2(p) })
    }

    /// Structural-index walk: get '=' and SOH bitmasks for W bytes at a time,
    /// then visit the delimiters with trailing_zeros instead of testing every byte.
    #[inline(always)]
    fn walk<const W: usize>(&mut self, frame: &[u8], body_len: usize, masks: impl Fn(*const u8) -> (u64, u64, u64)) -> Fast {
        const IN_TAG: usize = usize::MAX;
        let mut field_start = 0;
        let mut value_start = IN_TAG;
        let mut tag = 0u32;
        let mut field_idx = 0;
        // Last partial chunk is copied here. Spaces set no mask bits.
        let mut tail = [b' '; 32];

        let mut base = 0;
        while base < body_len {
            let p = if base + W <= body_len {
                frame[base..base + W].as_ptr()
            } else {
                let n = body_len - base;
                tail[..n].copy_from_slice(&frame[base..body_len]);
                tail.as_ptr()
            };
            let (soh, eq, bad) = masks(p);
            if bad != 0 {
                return Err(())
            }

            let mut bits = soh | eq;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                let pos = base + bit;
                let is_soh = (soh >> bit) & 1 != 0;
                bits &= bits - 1;

                if value_start == IN_TAG {
                    if is_soh {
                        return Err(())
                    }
                    tag = decode_tag(frame, field_start, pos - field_start).ok_or(())?;
                    value_start = pos + 1;
                } else if is_soh {
                    self.fast_field(&mut field_idx, tag, value_start, pos - value_start)?;
                    value_start = IN_TAG;
                    field_start = pos + 1;
                }
                // '=' inside a value is just part of the value.
            }
            base += W;
        }

        if value_start != IN_TAG || field_idx < HEADER_TAGS.len() { Err(()) } else { Ok(()) }
    }
}


#[cfg(test)]
macro_rules! parser_tests {
    ($name:ident, $mode:expr) => {
        mod $name {
    use super::*;

    // Same samples as the framer tests, with checksums from a real session.
    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";
    const LOGON: &[u8] = b"8=FIX.4.4\x019=75\x0135=A\x0134=1092\x0149=TESTBUY1\x0152=20180920-18:24:59.643\x0156=TESTSELL1\x0198=0\x01108=60\x0110=178\x01";

    /// Builds a frame from `fields` ('|' for SOH) after BodyLength, with a
    /// correct BodyLength and CheckSum.
    fn frame(fields: &str) -> Vec<u8> {
        let fields = fields.replace('|', "\x01");
        let mut out = format!("8=FIX.4.4\x019={}\x01{}", fields.len(), fields).into_bytes();
        let sum = out.iter().fold(0u32, |sum, &b| sum + b as u32) % 256;
        out.extend(format!("10={:03}\x01", sum).bytes());
        out
    }

    /// Same as `frame`, but the CheckSum is replaced by `checksum`.
    fn frame_with_checksum(fields: &str, checksum: &str) -> Vec<u8> {
        let mut out = frame(fields);
        out.truncate(out.len() - TRAILER_LEN);
        out.extend(format!("10={}\x01", checksum).bytes());
        out
    }

    fn parser() -> Box<Parser> {
        Box::new(Parser::with_mode($mode))
    }

    fn expect_message<'a>(outcome: ParseOutcome<'a>) -> Message<'a> {
        match outcome {
            ParseOutcome::Message(msg) => msg,
            ParseOutcome::Garbled(kind) => panic!("expected message, got Garbled({kind:?})"),
            ParseOutcome::Reject { tag, reason, .. } => panic!("expected message, got Reject({tag}, {reason:?})"),
        }
    }

    fn expect_garbled(outcome: ParseOutcome) -> GarbledKind {
        match outcome {
            ParseOutcome::Garbled(kind) => kind,
            ParseOutcome::Message(_) => panic!("expected Garbled, got message"),
            ParseOutcome::Reject { tag, reason, .. } => panic!("expected Garbled, got Reject({tag}, {reason:?})"),
        }
    }

    fn expect_reject<'a>(outcome: ParseOutcome<'a>) -> (Message<'a>, u16, RejectReason) {
        match outcome {
            ParseOutcome::Reject { partial, tag, reason } => (partial, tag, reason),
            ParseOutcome::Message(_) => panic!("expected Reject, got message"),
            ParseOutcome::Garbled(kind) => panic!("expected Reject, got Garbled({kind:?})"),
        }
    }

    #[test]
    fn parses_order() {
        let mut p = parser();
        let msg = expect_message(p.parse_frame(ORDER));
        assert_eq!(msg.raw, ORDER);
        assert_eq!(msg.get_str(8), Some("FIX.4.4"));
        assert_eq!(msg.get_int(9), Some(Ok(148)));
        assert_eq!(msg.get_msg_type(), Some(&b"D"[..]));
        assert_eq!(msg.get_int(34), Some(Ok(1080)));
        assert_eq!(msg.get_str(11), Some("636730640278898634"));
        assert_eq!(msg.get_str(60), Some("20180920-18:14:19.492"));
        assert_eq!(msg.get_str(10), Some("092"));
        assert_eq!(msg.get_str(58), None);
    }

    #[test]
    fn parses_logon() {
        let mut p = parser();
        let msg = expect_message(p.parse_frame(LOGON));
        assert_eq!(msg.get_msg_type(), Some(&b"A"[..]));
        assert_eq!(msg.get_int(108), Some(Ok(60)));
        assert_eq!(msg.get_str(10), Some("178"));
    }

    #[test]
    fn offsets_are_relative_to_frame_start() {
        let mut p = parser();
        let f = frame("35=0|34=7|");
        let msg = expect_message(p.parse_frame(&f));
        let value = msg.fields.get_slice(34, &f).unwrap();
        let offset = value.as_ptr() as usize - f.as_ptr() as usize;
        assert_eq!(&f[offset - 3..offset], b"34=");
    }

    #[test]
    fn fields_from_previous_frame_are_cleared() {
        let mut p = parser();
        expect_message(p.parse_frame(&frame("35=3|34=1|58=bad|")));
        let f = frame("35=0|34=2|");
        let msg = expect_message(p.parse_frame(&f));
        assert_eq!(msg.get_str(58), None);
        assert_eq!(msg.get_int(34), Some(Ok(2)));
    }

    #[test]
    fn parser_is_reusable_across_many_frames() {
        let mut p = parser();
        for seq in 1..=1000 {
            let f = frame(&format!("35=0|34={seq}|49=A|56=B|"));
            let msg = expect_message(p.parse_frame(&f));
            assert_eq!(msg.get_int(34), Some(Ok(seq)));
        }
    }

    #[test]
    fn checksum_mismatch_is_garbled() {
        let mut p = parser();
        let mut f = ORDER.to_vec();
        let len = f.len();
        f[len - 2] = b'3'; // 092 -> 093
        assert_eq!(
            expect_garbled(p.parse_frame(&f)),
            GarbledKind::ChecksumMismatch { expected: 93, actual: 92 },
        );
    }

    #[test]
    fn corrupted_body_is_garbled() {
        let mut p = parser();
        let mut f = ORDER.to_vec();
        let pos = f.windows(4).position(|w| w == b"MSFT").unwrap();
        f[pos] = b'N';
        assert!(matches!(expect_garbled(p.parse_frame(&f)), GarbledKind::ChecksumMismatch { .. }));
    }

    #[test]
    fn malformed_checksum_is_garbled() {
        let mut p = parser();
        for checksum in ["1a3", "256", "999"] {
            let f = frame_with_checksum("35=0|34=1|", checksum);
            assert_eq!(expect_garbled(p.parse_frame(&f)), GarbledKind::MalformedChecksum, "{checksum}");
        }
    }

    #[test]
    fn malformed_trailer_is_garbled() {
        let mut p = parser();
        let cases: [&[u8]; 5] = [
            b"",
            b"10=000\x01",
            b"8=FIX.4.4\x019=5\x0135=0\x01",
            b"8=FIX.4.4\x019=5\x0135=0\x0111=000\x01",
            b"8=FIX.4.4\x019=4\x0135=010=000\x01",
        ];
        for f in cases {
            assert_eq!(expect_garbled(p.parse_frame(f)), GarbledKind::MalformedTrailer, "{:?}", f);
        }
    }

    #[test]
    fn bad_field_with_bad_checksum_is_garbled() {
        let mut p = parser();
        let f = frame_with_checksum("35=0|34=1|58=|", "000");
        assert!(matches!(expect_garbled(p.parse_frame(&f)), GarbledKind::ChecksumMismatch { .. }));
    }

    #[test]
    fn empty_value_is_rejected() {
        let mut p = parser();
        let f = frame("35=D|34=5|58=|55=MSFT|");
        let (partial, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (58, RejectReason::TagWithoutValue));
        assert_eq!(partial.get_int(34), Some(Ok(5)));
        assert_eq!(partial.get_str(55), None);
    }

    #[test]
    fn invalid_tags_are_rejected() {
        let mut p = parser();
        for field in ["5x=1|", "=1|", "0=1|", "123456=1|", "70000=1|", "|"] {
            let f = frame(&format!("35=0|34=9|{field}"));
            let (partial, tag, reason) = expect_reject(p.parse_frame(&f));
            assert_eq!((tag, reason), (0, RejectReason::InvalidTag), "{field}");
            assert_eq!(partial.get_int(34), Some(Ok(9)), "{field}");
        }
    }

    #[test]
    fn tag_beyond_index_map_is_rejected() {
        let mut p = parser();
        let f = frame("35=0|34=1|20000=x|");
        let (_, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (20000, RejectReason::InvalidTag));
    }

    #[test]
    fn repeated_tag_is_rejected() {
        let mut p = parser();
        let f = frame("35=D|34=3|55=MSFT|55=AAPL|");
        let (partial, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (55, RejectReason::TagRepeated));
        assert_eq!(partial.get_str(55), Some("MSFT"));
    }

    #[test]
    fn control_character_in_value_is_rejected() {
        let mut p = parser();
        for value in ["a\tb", "a\x7fb", "caf\u{e9}"] {
            let f = frame(&format!("35=0|34=1|58={value}|"));
            let (_, tag, reason) = expect_reject(p.parse_frame(&f));
            assert_eq!((tag, reason), (58, RejectReason::IncorrectDataFormat), "{value:?}");
        }
    }

    #[test]
    fn msg_type_must_be_third() {
        let mut p = parser();
        let f = frame("34=1|35=0|");
        let (_, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (34, RejectReason::TagOutOfOrder));
    }

    #[test]
    fn header_tags_must_come_first() {
        let mut p = parser();
        let mut f = b"9=10\x018=FIX.4.4\x0135=0\x01".to_vec();
        let sum = f.iter().fold(0u32, |sum, &b| sum + b as u32) % 256;
        f.extend(format!("10={:03}\x01", sum).bytes());
        let (_, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (9, RejectReason::TagOutOfOrder));
    }

    #[test]
    fn checksum_inside_body_is_rejected() {
        let mut p = parser();
        let f = frame("35=0|10=000|34=1|");
        let (_, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (10, RejectReason::TagOutOfOrder));
    }

    #[test]
    fn missing_msg_type_is_rejected() {
        let mut p = parser();
        let mut f = b"8=FIX.4.4\x019=0\x01".to_vec();
        let sum = f.iter().fold(0u32, |sum, &b| sum + b as u32) % 256;
        f.extend(format!("10={:03}\x01", sum).bytes());
        let (_, tag, reason) = expect_reject(p.parse_frame(&f));
        assert_eq!((tag, reason), (35, RejectReason::RequiredTagMissing));
    }
}
    };
}

#[cfg(test)]
parser_tests!(scalar, Mode::Scalar);
#[cfg(test)]
parser_tests!(memchr_mode, Mode::Memchr);
#[cfg(test)]
parser_tests!(sse2, Mode::Sse2);
#[cfg(test)]
parser_tests!(avx2, Mode::Avx2);

// --- stage-by-stage timing hooks (used by benches::scan_stages) ---
pub mod experiment {
    use super::*;
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    /// 0 = masks only, 1 = + walk/decode_tag, 2 = + field checks without insert
    pub unsafe fn scan(frame: &[u8], level: u8) -> u64 {
        let body_len = frame.len() - TRAILER_LEN;
        let mut acc = 0u64;
        let mut tail = [b' '; 32];
        let (mut field_start, mut value_start, mut idx) = (0usize, usize::MAX, 0usize);
        let mut base = 0;
        while base < body_len {
            let p = if base + 32 <= body_len { frame[base..base + 32].as_ptr() } else {
                let n = body_len - base; tail[..n].copy_from_slice(&frame[base..body_len]); tail.as_ptr() };
            let (soh, eq, bad) = unsafe { simd::masks_avx2(p) };
            if level == 0 { acc += (soh | eq | bad).count_ones() as u64; base += 32; continue }
            let mut bits = soh | eq;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize; let pos = base + bit;
                let is_soh = (soh >> bit) & 1 != 0; bits &= bits - 1;
                if value_start == usize::MAX {
                    let tag = decode_tag(frame, field_start, pos - field_start).unwrap_or(0);
                    acc += tag as u64; value_start = pos + 1;
                } else if is_soh {
                    if level >= 2 {
                        let tag = acc as u16;
                        if (idx < 3 && tag == 9999) || tag == 10 || pos == value_start { return 0 }
                        idx += 1;
                    }
                    acc += (pos - value_start) as u64; value_start = usize::MAX; field_start = pos + 1;
                }
            }
            base += 32;
        }
        acc
    }
}
