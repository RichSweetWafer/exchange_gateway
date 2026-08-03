use crate::fix::message::FixMessage;


pub struct TestRequestView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}


impl<'a, const F: usize, const B: usize> TestRequestView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    pub fn test_request_id() -> Option<&'a [u8]> {
        msg.get_field(112)
    }
    
}