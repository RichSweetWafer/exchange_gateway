use crate::fix::message::FixMessage;

pub struct MarketDataView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}

impl<'a, const F: usize, const B: usize> MarketDataView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }
}