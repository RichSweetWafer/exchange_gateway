use crate::fix::message::FixMessage;
use crate::fix::converters::bytes_to_u64;

pub struct ResendRequestView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}


impl<'a, const F: usize, const B: usize> ResendRequestView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    pub fn first_message() -> Option<SeqNum> {
        bytes_to_u64(msg.get_field(7))
    }

    #[inline(always)]
    pub fn last_message() -> Option<SeqNum> {
        bytes_to_u64(msg.get_field(16))
    }
    
}