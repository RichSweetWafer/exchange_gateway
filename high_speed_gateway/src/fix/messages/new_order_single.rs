//! NewOrderSingle (D), and `Order`, the order terms D and G share.

use super::{
    Instrument, conditionally_required, decimal, enum_value, group, instrument, required_decimal, required_enum,
    required_str, required_time, time,
};
use crate::fix::app::AppReject;
use crate::fix::decode::Decimal;
use crate::fix::message::{Group, Message};
use crate::fix::spec::{self, HandlInst, OrdType, Side, TimeInForce, tags};

/// The order itself: NewOrderSingle (D), and the new terms of an
/// OrderCancelReplaceRequest (G).
#[derive(Clone, Copy)]
pub struct Order<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> Order<'a, 'buf> {
    /// Rejects (380=5) a missing Price for limit types, StopPx for stop types,
    /// or ExpireDate / ExpireTime for GTD.
    pub(super) fn decode(msg: &'a Message<'buf>) -> Result<Self, AppReject<'buf>> {
        let order = Self { msg };
        let id = Some(order.cl_ord_id());
        let ord_type = order.ord_type();
        if matches!(ord_type, OrdType::Limit | OrdType::StopLimit | OrdType::LimitOrBetter | OrdType::LimitWithOrWithout) {
            conditionally_required(order.has(tags::PRICE), id, "Price(44) required for this OrdType")?;
        }
        if matches!(ord_type, OrdType::Stop | OrdType::StopLimit) {
            conditionally_required(order.has(tags::STOP_PX), id, "StopPx(99) required for this OrdType")?;
        }
        if order.time_in_force() == Some(TimeInForce::GoodTillDate) {
            let expiry = order.has(tags::EXPIRE_DATE) || order.has(tags::EXPIRE_TIME);
            conditionally_required(expiry, id, "ExpireDate(432) or ExpireTime(126) required for GTD")?;
        }
        Ok(order)
    }

    /// For a message `decode` already accepted.
    pub(super) fn checked(msg: &'a Message<'buf>) -> Self {
        Self { msg }
    }

    fn has(&self, tag: u16) -> bool {
        self.msg.fields.position(tag).is_some()
    }

    pub fn cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::CL_ORD_ID)
    }

    /// NoPartyIDs (453): PartyID, PartyIDSource, PartyRole, nested NoPartySubIDs.
    pub fn parties(&self) -> Option<Group<'buf>> {
        // Same component in D and G, but each message has its own GroupSpec.
        let spec = match self.msg.get_msg_type() {
            Some(b"G") => &spec::ORDER_CANCEL_REPLACE_REQUEST_NO_PARTY_IDS,
            _ => &spec::NEW_ORDER_SINGLE_NO_PARTY_IDS,
        };
        group(self.msg, spec)
    }

    pub fn account(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ACCOUNT)
    }

    pub fn handl_inst(&self) -> Option<HandlInst> {
        enum_value(self.msg, tags::HANDL_INST, HandlInst::from_bytes)
    }

    pub fn exec_inst(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::EXEC_INST)
    }

    pub fn min_qty(&self) -> Option<Decimal> {
        decimal(self.msg, tags::MIN_QTY)
    }

    pub fn max_floor(&self) -> Option<Decimal> {
        decimal(self.msg, tags::MAX_FLOOR)
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

    pub fn order_qty(&self) -> Decimal {
        required_decimal(self.msg, tags::ORDER_QTY)
    }

    pub fn ord_type(&self) -> OrdType {
        required_enum(self.msg, tags::ORD_TYPE, OrdType::from_bytes)
    }

    /// Present for limit-priced order types.
    pub fn price(&self) -> Option<Decimal> {
        decimal(self.msg, tags::PRICE)
    }

    /// Present for stop order types.
    pub fn stop_px(&self) -> Option<Decimal> {
        decimal(self.msg, tags::STOP_PX)
    }

    pub fn currency(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::CURRENCY)
    }

    pub fn time_in_force(&self) -> Option<TimeInForce> {
        enum_value(self.msg, tags::TIME_IN_FORCE, TimeInForce::from_bytes)
    }

    /// LocalMktDate, YYYYMMDD.
    pub fn expire_date(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::EXPIRE_DATE)
    }

    pub fn expire_time(&self) -> Option<i64> {
        time(self.msg, tags::EXPIRE_TIME)
    }

    pub fn text(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::TEXT)
    }
}

debug_view!(Order {
    cl_ord_id, parties, account, handl_inst, exec_inst, min_qty, max_floor, instrument, side, transact_time,
    order_qty, ord_type, price, stop_px, currency, time_in_force, expire_date, expire_time, text,
});
