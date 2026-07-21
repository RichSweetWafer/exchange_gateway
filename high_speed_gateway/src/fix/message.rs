use crate::fix::views::{
    LogonView,
    LogoutView, 
    SessionControlView, 
    MarketDataView,
    MarketDataRejectView,
    AllocationReportView,
    PositionReportView,
    ExecutionReportView,
    OrderCancelRejectView,
};

// 64-byte struct for cache lines
#[repr(C)]
pub struct FieldDescriptor {
	pub tag: u16,    // FIX tag
	pub offset: u16, // Byte offset inside raw buffer
	pub len: u16,    // Value length in raw buffer
	pub d_type: u8,  // Data type enum
	pub _pad: u8,    // Explicit padding
}

#[repr(C)]
pub struct FixMessage<const MAX_FIELDS: usize = 32, const MAX_BUF: usize = 512> {
	pub seq_num: u64,      
	pub msg_type: u32,     
	pub field_count: u32,
	pub fields: [FieldDescriptor; MAX_FIELDS],
	pub len: usize,
	pub raw_buffer: [u8; MAX_BUF],
}


impl<'a, const F: usize, const B: usize> FixMessage<F, B> {
    #[inline(always)]
    pub fn view(&'a self) -> MessageView<'a, F, B> {
        match self.msg_type {
            // Session-level
            0x30..=0x34 /* 0 */ => MessageView::SessionControl(self),
            0x35 /* 5 */ => MessageView::Logout(self),
            0x41 /* A */ => MessageView::Logon(self),
        
            // Discovery
            0x57..=0x58 /* W, X */ => MessageView::MarketData(self),
            0x59 /* Y */ => MessageView::MarketDataReject(self),

            // Order
            0x38 /* 8 */ => MessageView::ExecutionReport(self),
            0x39 /* 9 */ => MessageView::OrderCancelReject(self),
            0x4150 /* AP */ => MessageView::PositionReport(self),
            0x4153 /* AS */ => MessageView::AllocationReport(self),

            _ => MessageView::Unsupported(self),
        }
    }

    #[inline(always)]
    pub fn get_field(&self, tag: u16) -> Option<&[u8]> {
        for i in 0..(self.field_count as usize) {
            if self.fields[i].tag == tag {
                let start = self.fields[i].offset as usize;
                let end = start + self.fields[i].len as usize;
                return Some(&self.raw_buffer[start..end]);
            }
        }
        None
    }
}



pub enum MessageView<'a, const F: usize, const B: usize> {
    // Session-level
    Logon(&'a LogonView<F, B>),
    Logout(&'a LogoutView<F, B>),
    //// Heartbeat, TestRequest, Resend, Reject, SequenceReset 
    SessionControl(&'a SessionControlView<F, B>),

    // Discovery
    MarketData(&'a MarketDataView<F, B>), // Snapshots and Incremental
    MarketDataReject(&'a MarketDataRejectView<F, B>),
    // MarketDataRequest(&'a FixMessage<F, B>),
    // SecurityDefinitionsRequest(&'a FixMessage<F, B>),
    // SecurityDefinitions(&'a FixMessage<F, B>),

    // Pre-post-trade
    AllocationReport(&'a AllocationReportView<F, B>),
    PositionReport(&'a PositionReportView<F, B>),

    // Order
    ExecutionReport(&'a ExecutionReportView<F, B>),
    OrderCancelReject(&'a OrderCancelRejectView<F, B>),
    // #[cfg(feature = "exchange")]
    // NewOrderSingle(&'a FixMessage<F, B>),
    
    Unsupported(&'a FixMessage<F, B>),
}