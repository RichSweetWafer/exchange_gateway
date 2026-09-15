use crate::fix::message::Message;

pub struct AllocationReportView<'a, const F: usize, const B: usize> {
    msg: &'a Message<F, B>
}

impl<'a, const F: usize, const B: usize> AllocationReportView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a Message<F, B>) -> Self {
        Self { msg }
    }
}