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
    pub fn encryption_method(&self) -> Option<&'a [u8]> {
        self.msg.get_field(98)
    }

    #[inline(always)]
    pub fn heartbeat_interval(&self) -> Option<&'a [u8]> {
        self.msg.get_field(108)
    }

    #[inline(always)]
    pub fn raw_data_length(&self) -> Option<&'a [u8]> {
        self.msg.get_field(95)
    }

    #[inline(always)]
    pub fn raw_data(&self) -> Option<&'a [u8]> {
        self.msg.get_field(96)
    }

    #[inline(always)]
    pub fn should_reset_sequences(&self) -> Option<&'a [u8]> {
        self.msg.get_field(141)
    }

    #[inline(always)]
    pub fn next_expected_sequence(&self) -> Option<&'a [u8]> {
        self.msg.get_field(554)
    }

    #[inline(always)]
    pub fn max_message_size(&self) -> Option<&'a [u8]> {
        self.msg.get_field(554)
    }

    // TODO:
    // NoMsgTypes - 384 - number of supported message types, listed further
    // RefMsgType - 372 - supported message type
    // MsgDirection - 385 - supported message type direction 
    // from the point of view of the sender of the Logon

    #[inline(always)]
    /// The session will be considered for testing
    pub fn test_message(&self) -> Option<&'a [u8]> {
        self.msg.get_field(464)
    }

    #[inline(always)]
    pub fn username(&self) -> Option<&'a [u8]> {
        self.msg.get_field(553)
    }

    #[inline(always)]
    pub fn password(&self) -> Option<&'a [u8]> {
        self.msg.get_field(554)
    }

    // There are also optional fields
}