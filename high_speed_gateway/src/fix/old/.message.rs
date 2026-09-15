use crate::fix::views::{
    LogonView,
    LogoutView, 
    HeartbeatView,
    TestRequestView,
    ResendRequestView,
    RejectView, 
    MarketDataView,
    MarketDataRejectView,
    AllocationReportView,
    PositionReportView,
    ExecutionReportView,
    OrderCancelRejectView,
};


pub enum MessageView<'a, const F: usize, const B: usize> {
    // Session-level
    Logon(&'a LogonView<F, B>),
    Logout(&'a LogoutView<F, B>),
    //// Heartbeat, TestRequest, Resend, Reject, SequenceReset 
    SessionControl(&'a SessionControlView<F, B>),

    // Discovery
    MarketData(&'a MarketDataView<F, B>), // Snapshots and Incremental
    MarketDataReject(&'a MarketDataRejectView<F, B>),
    // MarketDataRequest(&'a Message<F, B>),
    // SecurityDefinitionsRequest(&'a Message<F, B>),
    // SecurityDefinitions(&'a Message<F, B>),

    // Pre-post-trade
    AllocationReport(&'a AllocationReportView<F, B>),
    PositionReport(&'a PositionReportView<F, B>),

    // Order
    ExecutionReport(&'a ExecutionReportView<F, B>),
    OrderCancelReject(&'a OrderCancelRejectView<F, B>),
    // #[cfg(feature = "exchange")]
    // NewOrderSingle(&'a Message<F, B>),
    
    Unsupported(&'a Message<F, B>),
}