use crate::fix::{FixVersion, recv_buffer::RecvBuffer};
use memchr::memmem;

const FIX_SEP: u8 = b'\x01';
/// "10=" + 3 digits + SOH
const TRAILER_LEN: usize = 7;
const MAX_BODY_LENGTH_DIGITS: usize = 7;

/// Splits the receive stream into whole FIX messages.
///
/// Only partial structure is checked: BeginString, BodyLength and the shape of
/// the `10=xxx` trailer. Fields and the checksum value are left to the parser.
pub struct Framer {
    pub fix_version: FixVersion,
    header_finder: memmem::Finder<'static>,
    header_len: usize,
    /// Length of the frame at the front of the buffer once its BodyLength is
    /// known, so a partial message isn't re-parsed on every read.
    pending_len: Option<usize>,
}

pub enum FramerResult<'a> {
    /// Nothing buffered.
    None,
    /// No complete message yet. Read more and call again.
    NeedMore,
    /// One message, from `8=` through the checksum's SOH.
    /// Consume buffer once it's handled, before the next read.
    Frame {
        buffer: &'a [u8],
    }
}

enum Header {
    NeedMore,
    Malformed,
    Complete(usize),
}

impl Framer {
    const HEADER_PREFIX: &[u8] = b"8=FIX.";
    const BODY_LENGTH_PREFIX: &[u8] = b"9=";

    pub fn new(fix_version: FixVersion) -> Self {
        let mut header_needle = [0u8; 16];
        header_needle[..Self::HEADER_PREFIX.len()].copy_from_slice(Self::HEADER_PREFIX);

        let version_bytes = fix_version.as_bytes();
        let version_end = Self::HEADER_PREFIX.len() + version_bytes.len();
        header_needle[Self::HEADER_PREFIX.len()..version_end].copy_from_slice(version_bytes);
        header_needle[version_end] = FIX_SEP;
        let header_len = version_end + 1;

        let header_finder = memmem::Finder::new(&header_needle[..header_len]).into_owned();

        Self {
            fix_version,
            header_finder,
            header_len,
            pending_len: None,
        }
    }

    /// Returns the next complete message at the front of `recv`. Bytes before
    /// a header, and headers that turn out to be malformed, are consumed.
    pub fn frame<'a, const N: usize>(&mut self, recv: &'a mut RecvBuffer<N>) -> FramerResult<'a> {
        
        let frame_len = loop {
            if recv.is_empty() {
                self.pending_len = None;
                return FramerResult::None
            }
            let frame_len = match self.pending_len {
                Some(len) => len,
                None => { // If len is not known, find a header's start
                    let buf = recv.data();
                    match self.header_finder.find(buf) {
                        Some(n) => recv.consume(n),
                        None => {
                            // Keep only a tail that may be the start of a split header.
                            let keep = self.split_header_len(buf);
                            recv.consume(buf.len() - keep);
                            return FramerResult::NeedMore
                        }
                    }

                    match self.parse_frame_len(recv.data(), N) {
                        Header::NeedMore => return FramerResult::NeedMore,
                        Header::Malformed => {
                            // Step past this "8=FIX" and look for the next one.
                            recv.consume(1);
                            continue
                        }
                        Header::Complete(len) => {
                            self.pending_len = Some(len);
                            len
                        }
                    }
                }
            };

            let buf = recv.data();
            if buf.len() < frame_len {
                return FramerResult::NeedMore
            }

            self.pending_len = None;
            if Self::trailer_ok(&buf[frame_len - TRAILER_LEN - 1..frame_len]) {
                break frame_len
            }
            // BodyLength lied. Resync from the byte after this header start.
            recv.consume(1);
        };

        FramerResult::Frame {
            buffer: &recv.data()[..frame_len],
        }
    }

    /// Length of the longest suffix of `buf` that is a proper prefix of the header.
    #[inline(always)]
    fn split_header_len(&self, buf: &[u8]) -> usize {
        let needle = self.header_finder.needle();
        let max = buf.len().min(self.header_len - 1);
        (1..=max)
            .rev()
            .find(|&n| buf[buf.len() - n..] == needle[..n])
            .unwrap_or(0)
    }

    /// `buf` starts at the BeginString header. Parses "9=<len>\x01" after it
    /// and returns the length of the whole message.
    #[inline(always)]
    fn parse_frame_len(&self, buf: &[u8], capacity: usize) -> Header {
        let rest = &buf[self.header_len..];

        let prefix_len = rest.len().min(Self::BODY_LENGTH_PREFIX.len());
        if rest[..prefix_len] != Self::BODY_LENGTH_PREFIX[..prefix_len] {
            return Header::Malformed
        }
        if prefix_len < Self::BODY_LENGTH_PREFIX.len() {
            return Header::NeedMore
        }

        let mut body_len = 0usize;
        for (i, &b) in rest[Self::BODY_LENGTH_PREFIX.len()..].iter().enumerate() {
            match b {
                b'0'..=b'9' if i < MAX_BODY_LENGTH_DIGITS => {
                    body_len = body_len * 10 + (b - b'0') as usize;
                }
                FIX_SEP if i > 0 => {
                    let frame_len = self.header_len
                        + Self::BODY_LENGTH_PREFIX.len()
                        + i + 1
                        + body_len
                        + TRAILER_LEN;
                    // A frame bigger than the buffer could never complete.
                    if body_len == 0 || frame_len > capacity {
                        return Header::Malformed
                    }
                    return Header::Complete(frame_len)
                }
                _ => return Header::Malformed,
            }
        }

        Header::NeedMore
    }

    /// `tail` is the SOH closing the body followed by "10=ddd\x01".
    #[inline(always)]
    fn trailer_ok(tail: &[u8]) -> bool {
        tail[0] == FIX_SEP
            && &tail[1..4] == b"10="
            && tail[4..7].iter().all(u8::is_ascii_digit)
            && tail[7] == FIX_SEP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 4096 * 4;

    const ORDER: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";
    const LOGON: &[u8] = b"8=FIX.4.4\x019=75\x0135=A\x0134=1092\x0149=TESTBUY1\x0152=20180920-18:24:59.643\x0156=TESTSELL1\x0198=0\x01108=60\x0110=178\x01";

    fn push(recv: &mut RecvBuffer<SIZE>, bytes: &[u8]) {
        recv.spare_mut()[..bytes.len()].copy_from_slice(bytes);
        recv.commit(bytes.len());
    }

    /// Frames everything buffered, consuming each message.
    fn drain(framer: &mut Framer, recv: &mut RecvBuffer<SIZE>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let FramerResult::Frame { buffer } = framer.frame(recv) {
            let len = buffer.len();
            out.push(buffer.to_vec());
            recv.consume(len);
        }
        out
    }

    fn setup() -> (Framer, RecvBuffer<SIZE>) {
        (Framer::new(FixVersion::Fix44), RecvBuffer::new().unwrap())
    }

    #[test]
    fn empty_buffer() {
        let (mut framer, mut recv) = setup();
        assert!(matches!(framer.frame(&mut recv), FramerResult::None));
    }

    #[test]
    fn single_message() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, ORDER);
        assert_eq!(drain(&mut framer, &mut recv), vec![ORDER.to_vec()]);
        assert!(recv.is_empty());
    }

    #[test]
    fn back_to_back_messages() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, ORDER);
        push(&mut recv, LOGON);
        push(&mut recv, ORDER);
        assert_eq!(drain(&mut framer, &mut recv), vec![ORDER.to_vec(), LOGON.to_vec(), ORDER.to_vec()]);
    }

    #[test]
    fn byte_at_a_time() {
        let (mut framer, mut recv) = setup();
        let stream = [ORDER, LOGON].concat();
        let mut out = Vec::new();
        for b in &stream {
            push(&mut recv, std::slice::from_ref(b));
            out.extend(drain(&mut framer, &mut recv));
        }
        assert_eq!(out, vec![ORDER.to_vec(), LOGON.to_vec()]);
        assert!(recv.is_empty());
    }

    #[test]
    fn skips_leading_garbage() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, b"\x01\x01garbage8=FIX.4.2\x018=FI");
        assert!(matches!(framer.frame(&mut recv), FramerResult::NeedMore));
        // Only the possible start of a header is kept.
        assert_eq!(recv.data(), b"8=FI");
        push(&mut recv, &LOGON[4..]);
        assert_eq!(drain(&mut framer, &mut recv), vec![LOGON.to_vec()]);
    }

    #[test]
    fn resyncs_after_bad_body_length() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, b"8=FIX.4.4\x019=12x\x01");
        push(&mut recv, b"8=FIX.4.4\x0135=0\x01");
        push(&mut recv, LOGON);
        assert_eq!(drain(&mut framer, &mut recv), vec![LOGON.to_vec()]);
    }

    #[test]
    fn resyncs_when_body_length_is_wrong() {
        let (mut framer, mut recv) = setup();
        // BodyLength 70 lands short of the trailer.
        let bad = [&LOGON[..12], b"70", &LOGON[14..]].concat();
        push(&mut recv, &bad);
        push(&mut recv, ORDER);
        assert_eq!(drain(&mut framer, &mut recv), vec![ORDER.to_vec()]);
    }

    #[test]
    fn rejects_frame_larger_than_buffer() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, b"8=FIX.4.4\x019=999999\x01");
        push(&mut recv, LOGON);
        assert_eq!(drain(&mut framer, &mut recv), vec![LOGON.to_vec()]);
    }

    #[test]
    fn waits_for_split_body_length() {
        let (mut framer, mut recv) = setup();
        push(&mut recv, &ORDER[..12]);
        assert!(matches!(framer.frame(&mut recv), FramerResult::NeedMore));
        push(&mut recv, &ORDER[12..]);
        assert_eq!(drain(&mut framer, &mut recv), vec![ORDER.to_vec()]);
    }

    #[test]
    fn frame_across_physical_end() {
        let (mut framer, mut recv) = setup();
        recv.commit(SIZE - 20);
        recv.consume(SIZE - 20);
        push(&mut recv, ORDER);
        assert_eq!(drain(&mut framer, &mut recv), vec![ORDER.to_vec()]);
    }
}
