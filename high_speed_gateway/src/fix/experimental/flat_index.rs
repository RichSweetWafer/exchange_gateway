//! EXPERIMENTAL copy of `src/fix/flat_index.rs` with two additions:
//! - `Writer`: borrows the index for one message, keeps `len` in a register and
//!   commits it once on drop. Pushes compile without panicking bounds checks and
//!   without the per-field store/reload of `len`.
//! - `push_all`: batch push with a local length.
//!
//! Measured (VirtualBox VM, Ryzen 5 4600H, release, pinned): 16 back-to-back
//! pushes take 90 ns -> 20-25 ns, but end to end in the parser the gain
//! disappears (see `parser_tape`), because scanning hides the store latency.

use std::num::ParseIntError;

#[cfg(debug_assertions)]
use tracing::trace;

const MAX_TAG: usize = 16_384;
/// Shortest possible field: "1=x<SOH>".
pub const MIN_FIELD_LEN: usize = 4;
/// Receive buffer size. The framer never hands over a bigger frame.
pub const RECV_SIZE: usize = 4096 * 4;
/// A frame can't hold more fields than this, so a tape this long never fills.
pub const TAPE_LEN: usize = RECV_SIZE / MIN_FIELD_LEN;


/// One field on the tape. Offset is relative to the frame start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Field {
    pub offset: u32,
    pub length: u16,
    pub tag: u16,
}
const _: () = assert!(size_of::<Field>() == 8);


#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PushError {
    /// Tag doesn't fit the per-tag index.
    TagOutOfRange,
    /// Tape is full. Can't happen with N = TAPE_LEN and framer-checked frames.
    Full,
}


/// Every field of a message in wire order (the tape), plus a per-tag index
/// of each tag's first occurrence.
///
/// `first` is a sparse set: a slot only counts if it points inside the
/// current tape at a field with the same tag. Stale slots from earlier
/// messages fail that check, so clearing is just `len = 0`.
#[derive(Debug, Clone, Copy)]
pub struct FlatIndex<const N: usize> {
    first: [u16; MAX_TAG],
    tape: [Field; N],
    len: u16,
}

impl<const N: usize> Default for FlatIndex<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> FlatIndex<N> {
    // Tape positions are stored as u16.
    const CAPACITY_OK: () = assert!(N > 0 && N < u16::MAX as usize);

    pub fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            first: [0; MAX_TAG],
            tape: [Field::default(); N],
            len: 0,
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Appends a field. Only the first occurrence of a tag is indexed,
    /// repeats are reachable through `tape()`.
    #[inline(always)]
    pub fn push(&mut self, tag: u16, offset: u32, length: u16) -> Result<(), PushError> {
        let tag_idx = tag as usize;
        let pos = self.len as usize;

        #[cfg(debug_assertions)]
        trace!(tag, offset, length, "pushing: ");

        if tag_idx >= MAX_TAG {
            return Err(PushError::TagOutOfRange);
        }
        if pos == N {
            return Err(PushError::Full);
        }

        if self.position(tag).is_none() {
            self.first[tag_idx] = pos as u16;
        }
        self.tape[pos] = Field { offset, length, tag };
        self.len += 1;

        Ok(())
    }

    /// Tape position of the tag's first occurrence.
    #[inline(always)]
    pub fn position(&self, tag: u16) -> Option<usize> {
        let pos = *self.first.get(tag as usize)? as usize;
        (pos < self.len as usize && self.tape[pos].tag == tag).then_some(pos)
    }

    #[inline(always)]
    pub fn get(&self, tag: u16) -> Option<Field> {
        self.position(tag).map(|pos| self.tape[pos])
    }

    /// All fields of the current message, in wire order.
    #[inline(always)]
    pub fn tape(&self) -> &[Field] {
        &self.tape[..self.len as usize]
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn get_slice<'a>(&self, tag: u16, buffer: &'a [u8]) -> Option<&'a [u8]> {
        let field = self.get(tag)?;
        let start = field.offset as usize;
        let end = start + field.length as usize;
        Some(&buffer[start..end])
    }

    /// Values are checked to be printable ASCII by the parser.
    #[inline(always)]
    pub fn get_str<'a>(&self, tag: u16, buffer: &'a [u8]) -> Option<&'a str> {
        self.get_slice(tag, buffer).map(|slice| unsafe { str::from_utf8_unchecked(slice) })
    }

    #[inline(always)]
    pub fn get_int(&self, tag: u16, buffer: &[u8]) -> Option<Result<i64, ParseIntError>> {
        self.get_str(tag, buffer).map(|s| s.parse::<i64>())
    }
}




// --- batch push with the length in a local (used by benches::flat_index_store) ---
impl<const N: usize> FlatIndex<N> {
    #[inline(always)]
    pub fn push_all(&mut self, fields: &[(u16, u32, u16)]) -> Result<(), PushError> {
        let mut len = self.len as usize;
        for &(tag, offset, length) in fields {
            let tag_idx = tag as usize;
            if tag_idx >= MAX_TAG || len == N {
                self.len = len as u16;
                return Err(if len == N { PushError::Full } else { PushError::TagOutOfRange });
            }
            let p = self.first[tag_idx] as usize;
            // p < len <= N, so the tape index can't go out of bounds; masking lets the compiler see it.
            if !(p < len && self.tape[p % N].tag == tag) {
                self.first[tag_idx] = len as u16;
            }
            self.tape[len % N] = Field { offset, length, tag };
            len += 1;
        }
        self.len = len as u16;
        Ok(())
    }
}


// --- writer that keeps the length in a register (used by parser_tape) ---

/// Borrows the index for one message. `len` lives in a local, so pushes don't
/// round-trip it through memory; it's written back once, on drop.
pub struct Writer<'a, const N: usize> {
    first: &'a mut [u16; MAX_TAG],
    tape: &'a mut [Field; N],
    out_len: &'a mut u16,
    len: usize,
}

impl<const N: usize> FlatIndex<N> {
    /// Starts a new message: clears the index and hands out a writer.
    #[inline(always)]
    pub fn writer(&mut self) -> Writer<'_, N> {
        self.len = 0;
        Writer { first: &mut self.first, tape: &mut self.tape, out_len: &mut self.len, len: 0 }
    }
}

impl<const N: usize> Writer<'_, N> {
    #[inline(always)]
    pub fn push(&mut self, tag: u16, offset: u32, length: u16) -> Result<(), PushError> {
        let tag_idx = tag as usize;
        let len = self.len;
        if tag_idx >= MAX_TAG {
            return Err(PushError::TagOutOfRange);
        }
        if len >= N {
            return Err(PushError::Full);
        }
        // Sparse-set check; p < len < N, so neither index can panic.
        let p = self.first[tag_idx] as usize;
        if !(p < len && self.tape[p].tag == tag) {
            self.first[tag_idx] = len as u16;
        }
        self.tape[len] = Field { offset, length, tag };
        self.len = len + 1;
        Ok(())
    }
}

impl<const N: usize> Drop for Writer<'_, N> {
    #[inline(always)]
    fn drop(&mut self) {
        *self.out_len = self.len as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Index = FlatIndex<64>;

    fn index() -> Box<Index> {
        Box::new(Index::new())
    }

    /// Indexes a '|'-separated message and returns it with SOH in place of '|'.
    fn push_all(ix: &mut Index, msg: &str) -> Vec<u8> {
        let mut offset = 0;
        for field in msg.split('|').filter(|f| !f.is_empty()) {
            let (tag, value) = field.split_once('=').unwrap();
            let value_offset = offset + tag.len() + 1;
            ix.push(tag.parse().unwrap(), value_offset as u32, value.len() as u16).unwrap();
            offset += field.len() + 1;
        }
        msg.replace('|', "\x01").into_bytes()
    }

    #[test]
    fn field_is_8_bytes() {
        assert_eq!(size_of::<Field>(), 8);
    }

    #[test]
    fn tape_holds_every_message() {
        // Smallest fields packed into a full receive buffer.
        assert_eq!(TAPE_LEN * MIN_FIELD_LEN, RECV_SIZE);
    }

    #[test]
    fn lookup_by_tag() {
        let mut ix = index();
        let raw = push_all(&mut ix, "35=D|34=1080|55=MSFT|");
        assert_eq!(ix.get_str(35, &raw), Some("D"));
        assert_eq!(ix.get_int(34, &raw), Some(Ok(1080)));
        assert_eq!(ix.get_slice(55, &raw), Some(&b"MSFT"[..]));
        assert_eq!(ix.get(58), None);
        assert_eq!(ix.position(55), Some(2));
    }

    #[test]
    fn first_occurrence_wins() {
        let mut ix = index();
        let raw = push_all(&mut ix, "35=W|268=2|269=0|270=1.5|269=1|270=1.6|");
        assert_eq!(ix.get_str(269, &raw), Some("0"));
        assert_eq!(ix.get_str(270, &raw), Some("1.5"));
        assert_eq!(ix.position(269), Some(2));
    }

    #[test]
    fn repeats_stay_on_tape() {
        let mut ix = index();
        let raw = push_all(&mut ix, "35=W|268=2|269=0|270=1.5|269=1|270=1.6|");
        let tags: Vec<u16> = ix.tape().iter().map(|f| f.tag).collect();
        assert_eq!(tags, [35, 268, 269, 270, 269, 270]);

        let prices: Vec<&[u8]> = ix.tape().iter()
            .filter(|f| f.tag == 270)
            .map(|f| &raw[f.offset as usize..(f.offset + f.length as u32) as usize])
            .collect();
        assert_eq!(prices, [&b"1.5"[..], &b"1.6"[..]]);
    }

    #[test]
    fn clear_empties_index() {
        let mut ix = index();
        push_all(&mut ix, "35=D|34=1|58=hi|");
        ix.clear();
        assert!(ix.is_empty());
        assert!(ix.tape().is_empty());
        for tag in [35, 34, 58] {
            assert_eq!(ix.get(tag), None, "{tag}");
        }
    }

    #[test]
    fn stale_slots_are_invalid_after_clear() {
        let mut ix = index();
        // 58 sits at position 2 in the first message.
        push_all(&mut ix, "35=3|34=1|58=bad|");
        ix.clear();

        // Position 2 is now covered again, but by a different tag.
        let raw = push_all(&mut ix, "35=0|34=2|112=t|");
        assert_eq!(ix.get(58), None);
        assert_eq!(ix.get_str(112, &raw), Some("t"));

        // Shorter message: position 2 is past the end.
        ix.clear();
        push_all(&mut ix, "35=0|");
        assert_eq!(ix.get(58), None);
        assert_eq!(ix.get(34), None);
    }

    #[test]
    fn stale_slot_does_not_hide_new_first_occurrence() {
        let mut ix = index();
        // 270 at position 3 in the first message.
        push_all(&mut ix, "35=W|268=1|269=0|270=1.5|");
        ix.clear();

        // Next message has 270 at position 1 and again at 3. The old slot
        // points at 3, which isn't the first occurrence any more.
        let raw = push_all(&mut ix, "35=W|270=9.9|269=0|270=1.6|");
        assert_eq!(ix.position(270), Some(1));
        assert_eq!(ix.get_str(270, &raw), Some("9.9"));
    }

    #[test]
    fn reusable_across_many_messages() {
        let mut ix = index();
        for seq in 1..=1000 {
            ix.clear();
            let raw = push_all(&mut ix, &format!("35=0|34={seq}|49=A|56=B|"));
            assert_eq!(ix.get_int(34, &raw), Some(Ok(seq)));
            assert_eq!(ix.len(), 4);
        }
    }

    #[test]
    fn out_of_range_tag_is_an_error() {
        let mut ix = index();
        assert_eq!(ix.push(MAX_TAG as u16, 0, 1), Err(PushError::TagOutOfRange));
        assert_eq!(ix.push(u16::MAX, 0, 1), Err(PushError::TagOutOfRange));
        assert!(ix.is_empty());
        assert_eq!(ix.get(u16::MAX), None);
    }

    #[test]
    fn full_tape_is_an_error() {
        let mut ix = Box::new(FlatIndex::<2>::new());
        ix.push(35, 0, 1).unwrap();
        ix.push(34, 0, 1).unwrap();
        assert_eq!(ix.push(49, 0, 1), Err(PushError::Full));
        assert_eq!(ix.len(), 2);
    }
}
