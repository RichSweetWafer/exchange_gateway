use crate::fix::message::FixMessage;


pub struct LogoutView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}



impl<'a, const F: usize, const B: usize> LogoutView<'a, F, B> {
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    pub fn text(&self) -> Option<&'a [u8]> {
        self.msg.get_field(58)
    }

    #[inline(always)]
    pub fn encoded_text_length(&self) -> Option<&'a [u8]> {
        self.msg.get_field(354)
    }

    #[inline(always)]
    pub fn encoded_text(&self) -> Option<&'a [u8]> {
        self.msg.get_field(355)
    }

}