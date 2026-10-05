//! ExecutionReport (8).

use super::{
    Instrument, conditionally_required, decimal, enum_value, group, instrument, required_decimal, required_enum,
    required_str, time,
};
use crate::fix::app::AppReject;
use crate::fix::decode::Decimal;
use crate::fix::message::{Group, Message};
use crate::fix::spec::{self, ExecType, OrdRejReason, OrdStatus, OrdType, Side, TimeInForce, tags};

#[derive(Clone, Copy)]
pub struct ExecutionReport<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> ExecutionReport<'a, 'buf> {
    /// Rejects (380=5) a fill without LastQty or LastPx.
    pub(super) fn decode(msg: &'a Message<'buf>) -> Result<Self, AppReject<'buf>> {
        let report = Self { msg };
        if report.exec_type() == ExecType::Trade {
            let id = Some(report.exec_id());
            conditionally_required(msg.fields.position(tags::LAST_QTY).is_some(), id, "LastQty(32) required on a fill")?;
            conditionally_required(msg.fields.position(tags::LAST_PX).is_some(), id, "LastPx(31) required on a fill")?;
        }
        Ok(report)
    }

    pub fn order_id(&self) -> &'buf str {
        required_str(self.msg, tags::ORDER_ID)
    }

    pub fn cl_ord_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::CL_ORD_ID)
    }

    pub fn orig_cl_ord_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ORIG_CL_ORD_ID)
    }

    /// NoPartyIDs (453).
    pub fn parties(&self) -> Option<Group<'buf>> {
        group(self.msg, &spec::EXECUTION_REPORT_NO_PARTY_IDS)
    }

    pub fn exec_id(&self) -> &'buf str {
        required_str(self.msg, tags::EXEC_ID)
    }

    pub fn exec_type(&self) -> ExecType {
        required_enum(self.msg, tags::EXEC_TYPE, ExecType::from_bytes)
    }

    pub fn ord_status(&self) -> OrdStatus {
        required_enum(self.msg, tags::ORD_STATUS, OrdStatus::from_bytes)
    }

    pub fn ord_rej_reason(&self) -> Option<OrdRejReason> {
        enum_value(self.msg, tags::ORD_REJ_REASON, OrdRejReason::from_bytes)
    }

    pub fn account(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ACCOUNT)
    }

    pub fn instrument(&self) -> Instrument<'a, 'buf> {
        instrument(self.msg)
    }

    pub fn side(&self) -> Side {
        required_enum(self.msg, tags::SIDE, Side::from_bytes)
    }

    pub fn order_qty(&self) -> Option<Decimal> {
        decimal(self.msg, tags::ORDER_QTY)
    }

    pub fn ord_type(&self) -> Option<OrdType> {
        enum_value(self.msg, tags::ORD_TYPE, OrdType::from_bytes)
    }

    pub fn price(&self) -> Option<Decimal> {
        decimal(self.msg, tags::PRICE)
    }

    pub fn stop_px(&self) -> Option<Decimal> {
        decimal(self.msg, tags::STOP_PX)
    }

    pub fn currency(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::CURRENCY)
    }

    pub fn time_in_force(&self) -> Option<TimeInForce> {
        enum_value(self.msg, tags::TIME_IN_FORCE, TimeInForce::from_bytes)
    }

    /// Present on fills (ExecType F).
    pub fn last_qty(&self) -> Option<Decimal> {
        decimal(self.msg, tags::LAST_QTY)
    }

    /// Present on fills (ExecType F).
    pub fn last_px(&self) -> Option<Decimal> {
        decimal(self.msg, tags::LAST_PX)
    }

    pub fn leaves_qty(&self) -> Decimal {
        required_decimal(self.msg, tags::LEAVES_QTY)
    }

    pub fn cum_qty(&self) -> Decimal {
        required_decimal(self.msg, tags::CUM_QTY)
    }

    pub fn avg_px(&self) -> Decimal {
        required_decimal(self.msg, tags::AVG_PX)
    }

    /// Nanoseconds since the Unix epoch.
    pub fn transact_time(&self) -> Option<i64> {
        time(self.msg, tags::TRANSACT_TIME)
    }

    pub fn text(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::TEXT)
    }
}

debug_view!(ExecutionReport {
    order_id, cl_ord_id, orig_cl_ord_id, parties, exec_id, exec_type, ord_status, ord_rej_reason, account,
    instrument, side, order_qty, ord_type, price, stop_px, currency, time_in_force, last_qty, last_px,
    leaves_qty, cum_qty, avg_px, transact_time, text,
});
