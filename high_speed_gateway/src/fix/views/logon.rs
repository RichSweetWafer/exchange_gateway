use crate::fix::message::FixMessage;


/// Supports heartbeat interval and encryption method retrieval
pub struct LogonView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}



impl<'a, const F: usize, const B: usize> LogonView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    pub fn heartbeat_interval(&self) -> Option<&'a [u8]> {
        self.msg.get_field(108)
    }

    #[inline(always)]
    pub fn encryption_method(&self) -> Option<&'a [u8]> {
        self.msg.get_field(98)
    }

    // There are also optional fields
}