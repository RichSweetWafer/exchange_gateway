use crate::fix::message::FixMessage;


pub struct SessionControlView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}



impl<'a, const F: usize, const B: usize> SessionControlView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    // There are also optional fields
}