use super::decode;
use super::flat_index::{Field, FlatIndex, TAPE_LEN};
use super::spec::{GroupSpec, SessionRejectReason};
use std::num::ParseIntError;

const MGS_TYPE_TAG: u16 = 35;


#[derive(Clone, Copy)]
#[repr(C)]
pub struct Message<'buf> {
    pub raw: &'buf [u8],
    pub fields: &'buf FlatIndex<TAPE_LEN>,
}


impl<'buf> Message<'buf> {

    #[inline(always)]
    pub fn get_str(&self, tag: u16) -> Option<&'buf str> {
        self.fields.get_str(tag, self.raw)
    }

    #[inline(always)]
    pub fn get_int(&self, tag: u16) -> Option<Result<i64, ParseIntError>> {
        self.fields.get_int(tag, self.raw)
    }

    #[inline(always)]
    pub fn get_msg_type(&self) -> Option<&'buf [u8]> {
        self.fields.get_slice(35, self.raw)
    }

    #[inline(always)]
    pub fn get_boolean(&self, tag: u16) -> Option<bool> {
        match self.fields.get_slice(tag, self.raw) {
            Some([b'Y']) => Some(true),
            Some([b'N']) => Some(false),
            _ => None
        }
    }

    /// The repeating group `spec` describes, or None when its NumInGroup tag
    /// isn't in the message.
    #[inline(always)]
    pub fn group(&self, spec: &GroupSpec) -> Result<Option<Group<'buf>>, GroupError> {
        find_group(self.raw, self.fields.tape(), self.fields.position(spec.count_tag), spec)
    }

}


#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GroupError {
    /// NumInGroup isn't a number.
    BadCount { tag: u16 },
    /// NumInGroup doesn't match the entries found.
    CountMismatch { tag: u16, declared: usize, found: usize },
    /// An entry doesn't start with the delimiter.
    OutOfOrder { tag: u16 },
}

impl GroupError {
    /// RefTagID (371) for the Reject.
    pub fn ref_tag(&self) -> u16 {
        match *self {
            GroupError::BadCount { tag } | GroupError::CountMismatch { tag, .. } | GroupError::OutOfOrder { tag } => tag,
        }
    }

    /// SessionRejectReason (373) for the Reject.
    pub fn reject_reason(&self) -> SessionRejectReason {
        match self {
            GroupError::BadCount { .. } => SessionRejectReason::IncorrectDataFormatForValue,
            GroupError::CountMismatch { .. } => SessionRejectReason::IncorrectNumInGroupCountForRepeatingGroup,
            GroupError::OutOfOrder { .. } => SessionRejectReason::RepeatingGroupFieldsOutOfOrder,
        }
    }
}

/// A repeating group: a slice of the tape, nothing copied.
#[derive(Clone, Copy)]
pub struct Group<'buf> {
    raw: &'buf [u8],
    fields: &'buf [Field],
    delimiter: u16,
    len: usize,
}

impl std::fmt::Debug for Group<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Group").field("delimiter", &self.delimiter).field("len", &self.len).finish()
    }
}

impl<'buf> Group<'buf> {
    /// Number of entries.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Entries in wire order. Each one starts at a delimiter.
    #[inline(always)]
    pub fn entries(&self) -> impl Iterator<Item = Entry<'buf>> + 'buf {
        let (raw, delimiter) = (self.raw, self.delimiter);
        self.fields.chunk_by(move |_, b| b.tag != delimiter).map(move |fields| Entry { raw, fields })
    }

    /// All fields of the group, in wire order.
    #[inline(always)]
    pub fn fields(&self) -> &'buf [Field] {
        self.fields
    }
}

/// One entry of a group.
#[derive(Clone, Copy)]
pub struct Entry<'buf> {
    raw: &'buf [u8],
    fields: &'buf [Field],
}

impl<'buf> Entry<'buf> {
    /// Entries are a handful of fields, so a linear scan beats any index.
    #[inline(always)]
    pub fn get_slice(&self, tag: u16) -> Option<&'buf [u8]> {
        self.fields.iter().find(|f| f.tag == tag).map(|f| value(self.raw, f))
    }

    /// Values are checked to be printable ASCII by the parser.
    #[inline(always)]
    pub fn get_str(&self, tag: u16) -> Option<&'buf str> {
        self.get_slice(tag).map(|slice| unsafe { str::from_utf8_unchecked(slice) })
    }

    #[inline(always)]
    pub fn get_int(&self, tag: u16) -> Option<Result<i64, ParseIntError>> {
        self.get_str(tag).map(|s| s.parse::<i64>())
    }

    #[inline(always)]
    pub fn get_boolean(&self, tag: u16) -> Option<bool> {
        match self.get_slice(tag) {
            Some([b'Y']) => Some(true),
            Some([b'N']) => Some(false),
            _ => None
        }
    }

    /// Nested group inside this entry.
    #[inline(always)]
    pub fn group(&self, spec: &GroupSpec) -> Result<Option<Group<'buf>>, GroupError> {
        let start = self.fields.iter().position(|f| f.tag == spec.count_tag);
        find_group(self.raw, self.fields, start, spec)
    }

    /// All fields of the entry, in wire order.
    #[inline(always)]
    pub fn fields(&self) -> &'buf [Field] {
        self.fields
    }
}

#[inline(always)]
fn value<'buf>(raw: &'buf [u8], field: &Field) -> &'buf [u8] {
    let start = field.offset as usize;
    &raw[start..start + field.length as usize]
}

/// `fields` is the scope the group lives in: the whole tape, or the parent
/// entry for a nested group. `start` is the NumInGroup field's position in it.
#[inline(always)]
fn find_group<'buf>(raw: &'buf [u8], fields: &'buf [Field], start: Option<usize>, spec: &GroupSpec)
    -> Result<Option<Group<'buf>>, GroupError>
{
    let Some(pos) = start else { return Ok(None) };
    let declared = decode::to_usize(value(raw, &fields[pos]))
        .ok_or(GroupError::BadCount { tag: spec.count_tag })?;

    // The group ends at the first field that isn't one of its members.
    let after = &fields[pos + 1..];
    let span = after.iter().position(|f| !spec.is_member(f.tag)).unwrap_or(after.len());
    let body = &after[..span];

    if let Some(first) = body.first() && first.tag != spec.delimiter {
        return Err(GroupError::OutOfOrder { tag: first.tag })
    }
    let found = body.iter().filter(|f| f.tag == spec.delimiter).count();
    if found != declared {
        return Err(GroupError::CountMismatch { tag: spec.count_tag, declared, found })
    }

    Ok(Some(Group { raw, fields: body, delimiter: spec.delimiter, len: found }))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{Parser, ParseOutcome};
    use crate::fix::spec::TagRange;

    /// `members` as the TagRange build.rs would generate, `N` words wide.
    const fn member_bits<const N: usize>(base: u16, members: &[u16]) -> [u64; N] {
        let mut bits = [0u64; N];
        let mut i = 0;
        while i < members.len() {
            let bit = (members[i] - base) as usize;
            bits[bit / 64] |= 1 << (bit % 64);
            i += 1;
        }
        bits
    }

    const MD_ENTRIES: GroupSpec = GroupSpec { count_tag: 268, delimiter: 269, members: &[269, 270, 271, 272, 273],
        member_set: TagRange { base: 269, bits: &member_bits::<1>(269, &[269, 270, 271, 272, 273]) }, required: &[269], groups: &[] };
    const PARTY_SUB_IDS: GroupSpec = GroupSpec { count_tag: 802, delimiter: 523, members: &[523, 803],
        member_set: TagRange { base: 523, bits: &member_bits::<5>(523, &[523, 803]) }, required: &[], groups: &[] };
    const PARTIES: GroupSpec = GroupSpec { count_tag: 453, delimiter: 448, members: &[448, 447, 452, 802, 523, 803],
        member_set: TagRange { base: 447, bits: &member_bits::<6>(447, &[448, 447, 452, 802, 523, 803]) }, required: &[], groups: &[&PARTY_SUB_IDS] };

    /// Builds a frame from `fields` ('|' for SOH) after BodyLength.
    fn frame(fields: &str) -> Vec<u8> {
        let fields = fields.replace('|', "\x01");
        let mut out = format!("8=FIX.4.4\x019={}\x01{}", fields.len(), fields).into_bytes();
        let sum = out.iter().fold(0u32, |sum, &b| sum + b as u32) % 256;
        out.extend(format!("10={:03}\x01", sum).bytes());
        out
    }

    fn with_message<R>(fields: &str, check: impl FnOnce(Message) -> R) -> R {
        let raw = frame(fields);
        let mut parser = Box::new(Parser::new());
        match parser.parse_frame(&raw) {
            ParseOutcome::Message(msg) => check(msg),
            _ => panic!("frame didn't parse: {fields}"),
        }
    }

    #[test]
    fn market_data_entries() {
        with_message("35=W|55=MSFT|268=3|269=0|270=410.25|271=500|269=1|270=410.30|271=200|269=2|270=410.27|58=x|", |msg| {
            let book = msg.group(&MD_ENTRIES).unwrap().unwrap();
            assert_eq!(book.len(), 3);
            let entries: Vec<_> = book.entries().map(|e| (e.get_str(269), e.get_str(270), e.get_int(271))).collect();
            assert_eq!(entries, [
                (Some("0"), Some("410.25"), Some(Ok(500))),
                (Some("1"), Some("410.30"), Some(Ok(200))),
                (Some("2"), Some("410.27"), None),
            ]);
            // Fields after the group still resolve.
            assert_eq!(msg.get_str(58), Some("x"));
        });
    }

    #[test]
    fn absent_group_is_none() {
        with_message("35=0|34=1|", |msg| {
            assert!(msg.group(&MD_ENTRIES).unwrap().is_none());
        });
    }

    #[test]
    fn empty_group() {
        with_message("35=W|268=0|58=x|", |msg| {
            let book = msg.group(&MD_ENTRIES).unwrap().unwrap();
            assert!(book.is_empty());
            assert_eq!(book.entries().count(), 0);
        });
    }

    #[test]
    fn group_at_end_of_message() {
        with_message("35=W|268=1|269=0|270=1.5|", |msg| {
            let book = msg.group(&MD_ENTRIES).unwrap().unwrap();
            assert_eq!(book.entries().next().unwrap().get_str(270), Some("1.5"));
        });
    }

    #[test]
    fn nested_groups() {
        with_message("35=D|453=2|448=TRDR1|447=D|452=11|802=2|523=desk7|803=4|523=x|803=9|448=FIRM|447=D|452=13|55=MSFT|", |msg| {
            let parties = msg.group(&PARTIES).unwrap().unwrap();
            let parties: Vec<_> = parties.entries().collect();
            assert_eq!(parties.len(), 2);
            assert_eq!(parties[0].get_str(448), Some("TRDR1"));
            assert_eq!(parties[1].get_str(448), Some("FIRM"));

            let subs = parties[0].group(&PARTY_SUB_IDS).unwrap().unwrap();
            let subs: Vec<_> = subs.entries().map(|e| e.get_str(523)).collect();
            assert_eq!(subs, [Some("desk7"), Some("x")]);
            assert!(parties[1].group(&PARTY_SUB_IDS).unwrap().is_none());
            assert_eq!(msg.get_str(55), Some("MSFT"));
        });
    }

    #[test]
    fn count_mismatch() {
        with_message("35=W|268=2|269=0|270=1.5|58=x|", |msg| {
            let err = msg.group(&MD_ENTRIES).err().unwrap();
            assert_eq!(err, GroupError::CountMismatch { tag: 268, declared: 2, found: 1 });
            assert_eq!((err.ref_tag(), err.reject_reason()), (268, SessionRejectReason::IncorrectNumInGroupCountForRepeatingGroup));
        });
        with_message("35=W|268=1|269=0|269=1|", |msg| {
            assert!(matches!(msg.group(&MD_ENTRIES), Err(GroupError::CountMismatch { declared: 1, found: 2, .. })));
        });
    }

    #[test]
    fn entry_must_start_with_delimiter() {
        with_message("35=W|268=1|270=1.5|269=0|", |msg| {
            let err = msg.group(&MD_ENTRIES).err().unwrap();
            assert_eq!(err, GroupError::OutOfOrder { tag: 270 });
            assert_eq!((err.ref_tag(), err.reject_reason()), (270, SessionRejectReason::RepeatingGroupFieldsOutOfOrder));
        });
    }

    #[test]
    fn bad_count() {
        with_message("35=W|268=x|269=0|", |msg| {
            let err = msg.group(&MD_ENTRIES).err().unwrap();
            assert_eq!((err.ref_tag(), err.reject_reason()), (268, SessionRejectReason::IncorrectDataFormatForValue));
        });
    }
}
