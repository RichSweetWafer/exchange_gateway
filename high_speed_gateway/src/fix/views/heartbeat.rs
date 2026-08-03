use crate::fix::message::FixMessage;


pub struct HeartbeatView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}


impl<'a, const F: usize, const B: usize> HeartbeatView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    pub fn test_request_id() -> Option<&'a [u8]> {
        msg.get_field(112)
    }
    
}