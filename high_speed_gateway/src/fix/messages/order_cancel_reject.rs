//! OrderCancelReject (9).

use super::{enum_value, required_enum, required_str, time};
use crate::fix::message::Message;
use crate::fix::spec::{CxlRejReason, CxlRejResponseTo, OrdStatus, tags};

#[derive(Clone, Copy)]
pub struct OrderCancelReject<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> OrderCancelReject<'a, 'buf> {
    /// Nothing beyond the schema to check.
    pub(super) fn decode(msg: &'a Message<'buf>) -> Self {
        Self { msg }
    }

    pub fn order_id(&self) -> &'buf str {
        required_str(self.msg, tags::ORDER_ID)
    }

    pub fn cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::CL_ORD_ID)
    }

    pub fn orig_cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::ORIG_CL_ORD_ID)
    }

    pub fn ord_status(&self) -> OrdStatus {
        required_enum(self.msg, tags::ORD_STATUS, OrdStatus::from_bytes)
    }

    pub fn account(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ACCOUNT)
    }

    /// Nanoseconds since the Unix epoch.
    pub fn transact_time(&self) -> Option<i64> {
        time(self.msg, tags::TRANSACT_TIME)
    }

    pub fn response_to(&self) -> CxlRejResponseTo {
        required_enum(self.msg, tags::CXL_REJ_RESPONSE_TO, CxlRejResponseTo::from_bytes)
    }

    pub fn reason(&self) -> Option<CxlRejReason> {
        enum_value(self.msg, tags::CXL_REJ_REASON, CxlRejReason::from_bytes)
    }

    pub fn text(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::TEXT)
    }
}

debug_view!(OrderCancelReject {
    order_id, cl_ord_id, orig_cl_ord_id, ord_status, account, transact_time, response_to, reason, text,
});
