use crate::fix::message::Message;

pub struct MarketDataRejectView<'a, const F: usize, const B: usize> {
    msg: &'a Message<F, B>
}

impl<'a, const F: usize, const B: usize> MarketDataRejectView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a Message<F, B>) -> Self {
        Self { msg }
    }
}