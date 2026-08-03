use crate::fix::message::FixMessage;


pub struct RejectView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}

impl<'a, const F: usize, const B: usize> RejectView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    /// Sequence number of the rejected message
    pub fn seq_num() -> Option<&'a [u8]> {
        msg.get_field(45)
    }

    #[inline(always)]
    /// Tag of the rejected message
    pub fn tag() -> Option<&'a [u8]> {
        msg.get_field(371)
    }

    #[inline(always)]
    pub fn msg_type() -> Option<&'a [u8]> {
        msg.get_field(372)
    }

    #[inline(always)]
    pub fn reason() -> Option<&'a [u8]> {
        msg.get_field(373)
    }

    #[inline(always)]
    pub fn reason_text() -> Option<&'a [u8]> {
        msg.get_field(58)
    }
    
    #[inline(always)]
    pub fn encoded_reason_len() -> Option<&'a [u8]> {
        msg.get_field(354)
    }

    #[inline(always)]
    pub fn encoded_reason_text() -> Option<&'a [u8]> {
        msg.get_field(355)
    }

}