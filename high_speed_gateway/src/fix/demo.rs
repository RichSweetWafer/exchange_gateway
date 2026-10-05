//! Two small `Application`s for the CLI and the end-to-end tests.
//!
//! `OrderAcker` is a sell side with no book: it acknowledges every
//! NewOrderSingle, cancel and replace with an ExecutionReport. `OneOrder` is a
//! buy side that sends one NewOrderSingle on logon and records the reports.

use tracing::info;

use super::app::{Application, BusinessReject, Inbound};
use super::messages::AppMessage;
use super::decode::Decimal;
use super::encoder::{EncodeError, MessageWriter};
use super::session::{Outbox, Sender};
use super::spec::{ExecType, MsgType, OrdRejReason, OrdStatus, OrdType, Side, tags};

/// Acknowledges orders, cancels and replaces; rejects non-positive quantities.
#[derive(Default)]
pub struct OrderAcker {
    next_order_id: u64,
    next_exec_id: u64,
    /// ExecutionReports sent.
    pub reports: u64,
}

/// One ExecutionReport's variable part.
struct Report<'a> {
    cl_ord_id: &'a str,
    orig_cl_ord_id: Option<&'a str>,
    exec_type: ExecType,
    ord_status: OrdStatus,
    rej_reason: Option<OrdRejReason>,
    symbol: &'a str,
    side: Side,
    order_qty: Option<Decimal>,
    ord_type: Option<OrdType>,
    price: Option<Decimal>,
    /// LeavesQty: what's still open.
    leaves_qty: Decimal,
}

impl OrderAcker {
    fn report<O: Outbox>(&mut self, order_id: u64, r: Report, send: &mut Sender<'_, O>) {
        self.next_exec_id += 1;
        let exec_id = self.next_exec_id;
        let now = send.now();
        let sent = send.send(MsgType::ExecutionReport, |w: &mut MessageWriter| -> Result<(), EncodeError> {
            w.u64(tags::ORDER_ID, order_id)?.str(tags::CL_ORD_ID, r.cl_ord_id)?;
            if let Some(orig) = r.orig_cl_ord_id {
                w.str(tags::ORIG_CL_ORD_ID, orig)?;
            }
            w.u64(tags::EXEC_ID, exec_id)?
                .bytes(tags::EXEC_TYPE, r.exec_type.as_bytes())?
                .bytes(tags::ORD_STATUS, r.ord_status.as_bytes())?;
            if let Some(reason) = r.rej_reason {
                w.bytes(tags::ORD_REJ_REASON, reason.as_bytes())?;
            }
            w.str(tags::SYMBOL, r.symbol)?.bytes(tags::SIDE, r.side.as_bytes())?;
            if let Some(qty) = r.order_qty {
                w.decimal(tags::ORDER_QTY, qty)?;
            }
            if let Some(ord_type) = r.ord_type {
                w.bytes(tags::ORD_TYPE, ord_type.as_bytes())?;
            }
            if let Some(price) = r.price {
                w.decimal(tags::PRICE, price)?;
            }
            w.decimal(tags::LEAVES_QTY, r.leaves_qty)?
                .decimal(tags::CUM_QTY, Decimal::new(0, 0))?
                .decimal(tags::AVG_PX, Decimal::new(0, 0))?
                .timestamp(tags::TRANSACT_TIME, now)
                .map(|_| ())
        });
        match sent {
            Ok(_) => self.reports += 1,
            Err(e) => info!(?e, "execution report not sent"),
        }
    }

    fn order_id(&mut self, order_id: Option<&str>) -> u64 {
        // Our OrderIDs are numbers; anything else is someone else's order.
        order_id.and_then(|id| id.parse().ok()).unwrap_or_else(|| {
            self.next_order_id += 1;
            self.next_order_id
        })
    }
}

impl Application for OrderAcker {
    fn on_message<'buf, O: Outbox>(&mut self, msg: &Inbound<'_, 'buf>, send: &mut Sender<'_, O>)
        -> Result<(), BusinessReject<'buf>>
    {
        match &msg.view {
            AppMessage::NewOrderSingle(o) => {
                let order_id = self.order_id(None);
                let (cl_ord_id, symbol, side, order_qty) = (o.cl_ord_id(), o.instrument().symbol(), o.side(), o.order_qty());
                let accepted = order_qty.is_positive();
                info!(cl_ord_id, symbol, ?side, accepted, "order");
                let report = Report {
                    cl_ord_id,
                    orig_cl_ord_id: None,
                    exec_type: if accepted { ExecType::New } else { ExecType::Rejected },
                    ord_status: if accepted { OrdStatus::New } else { OrdStatus::Rejected },
                    rej_reason: (!accepted).then_some(OrdRejReason::IncorrectQuantity),
                    symbol,
                    side,
                    order_qty: Some(order_qty),
                    ord_type: Some(o.ord_type()),
                    price: o.price(),
                    leaves_qty: if accepted { order_qty } else { Decimal::new(0, 0) },
                };
                self.report(order_id, report, send);
            }
            AppMessage::OrderCancelRequest(c) => {
                let order_id = self.order_id(c.order_id());
                let report = Report {
                    cl_ord_id: c.cl_ord_id(),
                    orig_cl_ord_id: Some(c.orig_cl_ord_id()),
                    exec_type: ExecType::Canceled,
                    ord_status: OrdStatus::Canceled,
                    rej_reason: None,
                    symbol: c.instrument().symbol(),
                    side: c.side(),
                    order_qty: c.order_qty(),
                    ord_type: None,
                    price: None,
                    leaves_qty: Decimal::new(0, 0),
                };
                self.report(order_id, report, send);
            }
            AppMessage::OrderCancelReplaceRequest(r) => {
                let order_id = self.order_id(r.order_id());
                let o = r.order();
                let order_qty = o.order_qty();
                let report = Report {
                    cl_ord_id: o.cl_ord_id(),
                    orig_cl_ord_id: Some(r.orig_cl_ord_id()),
                    exec_type: ExecType::Replaced,
                    ord_status: OrdStatus::Replaced,
                    rej_reason: None,
                    symbol: o.instrument().symbol(),
                    side: o.side(),
                    order_qty: Some(order_qty),
                    ord_type: Some(o.ord_type()),
                    price: o.price(),
                    leaves_qty: order_qty,
                };
                self.report(order_id, report, send);
            }
            AppMessage::BusinessMessageReject(b) => info!(?b, "business reject"),
            // A sell side doesn't take reports.
            AppMessage::ExecutionReport(_) | AppMessage::OrderCancelReject(_) => return Err(BusinessReject::unsupported(msg)),
        }
        Ok(())
    }
}

/// The order `OneOrder` sends.
#[derive(Debug, Clone)]
pub struct OrderSpec {
    pub cl_ord_id: String,
    pub symbol: String,
    pub side: Side,
    pub qty: Decimal,
    /// Limit price; None sends a market order.
    pub price: Option<Decimal>,
}

/// Sends one order on logon and keeps what comes back.
pub struct OneOrder {
    order: OrderSpec,
    sent: bool,
    /// (ExecType, OrdStatus) of every ExecutionReport received.
    pub reports: Vec<(ExecType, OrdStatus)>,
}

impl OneOrder {
    pub fn new(order: OrderSpec) -> Self {
        Self { order, sent: false, reports: Vec::new() }
    }
}

impl Application for OneOrder {
    fn on_logon<O: Outbox>(&mut self, send: &mut Sender<'_, O>) {
        if self.sent {
            return
        }
        let o = &self.order;
        let now = send.now();
        let result = send.send(MsgType::NewOrderSingle, |w| {
            w.str(tags::CL_ORD_ID, &o.cl_ord_id)?
                .str(tags::SYMBOL, &o.symbol)?
                .bytes(tags::SIDE, o.side.as_bytes())?
                .timestamp(tags::TRANSACT_TIME, now)?
                .decimal(tags::ORDER_QTY, o.qty)?;
            match o.price {
                Some(price) => w.bytes(tags::ORD_TYPE, OrdType::Limit.as_bytes())?.decimal(tags::PRICE, price)?,
                None => w.bytes(tags::ORD_TYPE, OrdType::Market.as_bytes())?,
            };
            Ok(())
        });
        info!(cl_ord_id = o.cl_ord_id, ?result, "order sent");
        self.sent = result.is_ok();
    }

    fn on_message<'buf, O: Outbox>(&mut self, msg: &Inbound<'_, 'buf>, _send: &mut Sender<'_, O>)
        -> Result<(), BusinessReject<'buf>>
    {
        match &msg.view {
            AppMessage::ExecutionReport(e) => {
                let (exec_type, ord_status) = (e.exec_type(), e.ord_status());
                info!(order_id = e.order_id(), exec_id = e.exec_id(), ?exec_type, ?ord_status, "execution report");
                self.reports.push((exec_type, ord_status));
                Ok(())
            }
            AppMessage::BusinessMessageReject(b) => {
                info!(?b, "business reject");
                Ok(())
            }
            _ => Err(BusinessReject::unsupported(msg)),
        }
    }
}
