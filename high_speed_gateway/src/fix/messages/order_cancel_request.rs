//! OrderCancelRequest (F).

use super::{Instrument, decimal, group, instrument, required_enum, required_str, required_time};
use crate::fix::decode::Decimal;
use crate::fix::message::{Group, Message};
use crate::fix::spec::{self, Side, tags};

#[derive(Clone, Copy)]
pub struct OrderCancelRequest<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> OrderCancelRequest<'a, 'buf> {
    /// Nothing beyond the schema to check.
    pub(super) fn decode(msg: &'a Message<'buf>) -> Self {
        Self { msg }
    }

    pub fn orig_cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::ORIG_CL_ORD_ID)
    }

    pub fn order_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ORDER_ID)
    }

    pub fn cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::CL_ORD_ID)
    }

    pub fn account(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ACCOUNT)
    }

    /// NoPartyIDs (453).
    pub fn parties(&self) -> Option<Group<'buf>> {
        group(self.msg, &spec::ORDER_CANCEL_REQUEST_NO_PARTY_IDS)
    }

    pub fn instrument(&self) -> Instrument<'a, 'buf> {
        instrument(self.msg)
    }

    pub fn side(&self) -> Side {
        required_enum(self.msg, tags::SIDE, Side::from_bytes)
    }

    /// Nanoseconds since the Unix epoch.
    pub fn transact_time(&self) -> i64 {
        required_time(self.msg, tags::TRANSACT_TIME)
    }

    pub fn order_qty(&self) -> Option<Decimal> {
        decimal(self.msg, tags::ORDER_QTY)
    }

    pub fn text(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::TEXT)
    }
}

debug_view!(OrderCancelRequest {
    orig_cl_ord_id, order_id, cl_ord_id, account, parties, instrument, side, transact_time, order_qty, text,
});
