use std::num::ParseIntError;

use tracing::trace;

const MAX_TAG: usize = 16_384;


#[derive(Debug, Clone, Copy, Default)]
pub struct FieldRef {
    pub offset: u32,
    pub length: u32,
}


#[derive(Debug, Clone, Copy)]
pub struct FlatIndexMap {
    entries: [FieldRef; MAX_TAG],
}

impl Default for FlatIndexMap {
    fn default() -> Self {
        Self {
            entries: [FieldRef::default(); MAX_TAG],
        }
    }
}

impl FlatIndexMap {

    pub fn new() -> Self {
        Self {
            entries: [FieldRef::default(); MAX_TAG],
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.entries[..].fill(FieldRef::default());
    }


    #[inline(always)]
    pub fn insert(&mut self, tag: u16, offset: u32, length: u32) -> Result<(), &'static str> {
        let tag_idx = tag as usize;

        trace!(tag, offset, length, "inserting: ");
        
        if length == 0 {
            return Err("Value substring length must not be zero");
        }
        
        if tag_idx >= MAX_TAG {
            return Err("Unique tag value must not be larger than the allowed limit");
        }

        if self.entries[tag_idx].length != 0 {
            return Err("Unique tag value must be unique");
        }

        self.entries[tag_idx].offset = offset;
        self.entries[tag_idx].length = length;

        return Ok(());
    }


    #[inline(always)]
    pub fn get_str<'a>(&self, tag: u16, buffer: &'a [u8]) -> Option<&'a str> {
        let tag_idx = tag as usize;
        
        if tag_idx > MAX_TAG {
            return None;
        }

        let field = &self.entries[tag_idx];

        if field.length == 0 {
            return None;
        }

        let start = field.offset as usize;
        let end = start + (field.length as usize);

        return Some(unsafe { str::from_utf8_unchecked(&buffer[start..end]) })
    }


    #[inline(always)]
    pub fn get_int(&self, tag: u16, buffer: &[u8]) -> Option<Result<i64, ParseIntError>> {
        self.get_str(tag, buffer).map(|s| s.parse::<i64>())
    }

    // #[inline(always)]
    // pub fn get_ref(&self, tag: u16) -> Option<FieldRef> {
    //     let tag_idx = tag as usize;

    //     if tag_idx > MAX_TAG {
    //         return None;
    //     }

    //     let field = self.entries[tag_idx];

    //     if field.length == 0 {
    //         return None;
    //     }

    //     Some(field)
    // }
}