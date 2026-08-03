use crate::fix::message::FixMessage;


pub struct ResendRequestView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}


impl<'a, const F: usize, const B: usize> ResendRequestView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    pub fn first_message() -> Option<&'a [u8]> {
        msg.get_field(7)
    }

    pub fn last_message() -> Option<&'a [u8]> {
        msg.get_field(16)
    }
    
}