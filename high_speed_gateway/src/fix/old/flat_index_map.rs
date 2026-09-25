use std::num::ParseIntError;

#[cfg(debug_assertions)]
use tracing::trace;

const MAX_TAG: usize = 16_384;
const MAX_FIELDS: usize = 256;


#[derive(Debug, Clone, Copy, Default)]
pub struct FieldRef {
    pub offset: u32,
    pub length: u32,
}


#[derive(Debug, Clone, Copy)]
pub struct FlatIndexMap {
    entries: [FieldRef; MAX_TAG],
    touched: [u16; MAX_FIELDS],
    count: u16,
}

impl Default for FlatIndexMap {
    fn default() -> Self {
        Self {
            entries: [FieldRef::default(); MAX_TAG],
            touched: [0; MAX_FIELDS],
            count: 0,
        }
    }
}

impl FlatIndexMap {

    pub fn new() -> Self {
        Self {
            entries: [FieldRef::default(); MAX_TAG],
            touched: [0; MAX_FIELDS],
            count: 0,
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        for &tag in &self.touched[..self.count as usize] {
            self.entries[tag as usize] = FieldRef::default();
        }
        self.count = 0;
    }


    #[inline(always)]
    pub fn insert(&mut self, tag: u16, offset: u32, length: u32) -> Result<(), &'static str> {
        let tag_idx = tag as usize;

        #[cfg(debug_assertions)]
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

        if self.count as usize >= MAX_FIELDS {
            return Err("Too many fields in one message");
        }

        self.entries[tag_idx].offset = offset;
        self.entries[tag_idx].length = length;
        self.touched[self.count as usize] = tag;
        self.count += 1;

        return Ok(());
    }

    #[inline(always)]
    pub fn get_slice<'a>(&self, tag: u16, buffer: &'a [u8]) -> Option<&'a [u8]> {
                let tag_idx = tag as usize;
        
        if tag_idx >= MAX_TAG {
            return None;
        }

        let field = &self.entries[tag_idx];

        if field.length == 0 {
            return None;
        }

        let start = field.offset as usize;
        let end = start + (field.length as usize);

        return Some(&buffer[start..end])
    }

    #[inline(always)]
    pub fn get_str<'a>(&self, tag: u16, buffer: &'a [u8]) -> Option<&'a str> {        
        match self.get_slice(tag, buffer) {
            Some(slice) => { return Some(unsafe { str::from_utf8_unchecked(slice) })},
            None => { return None }
        }
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