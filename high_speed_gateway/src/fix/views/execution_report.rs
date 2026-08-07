use crate::fix::message::FixMessage;


/// ExecutionReport message is used to:
/// 1. confirm the receipt of an order
/// 2. confirm changes to an existing order (accept amend request)
/// 3. relay order status information
/// 4. relay fill information on working orders
/// 5. relay fill information on tradeable or restricted tradeable quotes
/// 6. reject orders
/// 7. report post-trade fees calculations associated with a trade
pub struct ExecutionReportView<'a, const F: usize, const B: usize> {
    msg: &'a FixMessage<F, B>
}

impl<'a, const F: usize, const B: usize> ExecutionReportView<'a, F, B> {
    
    #[inline(always)]
    pub(crate) fn new(msg: &'a FixMessage<F, B>) -> Self {
        Self { msg }
    }

    #[inline(always)]
    pub fn order_id(&self) -> Option<&'a [u8]> {
        self.msg.get_field(37)
    }

    #[inline(always)]
    pub fn exec_id(&self) -> Option<&'a [u8]> {
        self.msg.get_field(17)
    }

    #[inline(always)]
    pub fn exec_type(&self) -> Option<&'a [u8]> {
        self.msg.get_field(150)
    }

    #[inline(always)]
    pub fn order_status(&self) -> Option<&'a [u8]> {
        self.msg.get_field(39)
    }

    #[inline(always)]
    pub fn price(&self) -> Option<&'a [u8]> {
        self.msg.get_field(44)
    }

    #[inline(always)]
    pub fn symbol(&self) -> Option<&'a [u8]> {
        self.msg.get_field(55)
    }
}