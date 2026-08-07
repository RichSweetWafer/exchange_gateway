use super::flat_index_map::*;
use std::num::ParseIntError;

const MGS_TYPE_TAG: u16 = 35;


#[repr(C)]
pub struct FixMessage<'buf> {
    pub raw: &'buf [u8],
    pub tag_map: &'buf FlatIndexMap,
}


pub enum FixMessageView<'a, 'buf: 'a> {
    Logon(&'a LogonView<'buf>),
    Logout(&'a LogoutView<'buf>),
    Unknown(&'a FixMessage<'buf>),
}


impl<'buf> FixMessage<'buf> {

    #[inline(always)]
    pub fn get_str(&self, tag: u16) -> Option<&'buf str> {
        self.tag_map.get_str(tag, self.raw)
    }

    #[inline(always)]
    pub fn get_int(&self, tag: u16) -> Option<Result<i64, ParseIntError>> {
        self.tag_map.get_int(tag, self.raw)
    }

    #[inline(always)]
    pub fn into_view<'a>(&'a self) -> FixMessageView<'buf> {
        match self.get_str(MGS_TYPE_TAG) {
            Some("A") => {
                
                Message::Logon(view_ref)
            },
            _ => FixMessageView::Unknown(self),
        }
    }
}
