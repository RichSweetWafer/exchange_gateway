use super::flat_index_map::*;
use std::num::ParseIntError;

const MGS_TYPE_TAG: u16 = 35;


#[derive(Clone, Copy)]
#[repr(C)]
pub struct Message<'buf> {
    pub raw: &'buf [u8],
    pub fields: &'buf FlatIndexMap,
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

}
