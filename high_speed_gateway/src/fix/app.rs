//! Application layer: typed views of the order-flow messages (D, F, G, 8, 9, j)
//! and the `Application` trait the session hands them to.
//!
//! `decode` runs the strict schema check (`validate_body`), then builds the
//! view. Schema problems are session-level Rejects (35=3); rules the schema
//! can't express (Price on a limit order, LastQty / LastPx on a fill, ...) are
//! BusinessMessageRejects (35=j), as are the application's own refusals.

use super::decode::{self, Decimal};
use super::message::{Group, Message};
use super::session::{Outbox, Sender};
use super::spec::{
    self, BusinessRejectReason, CxlRejReason, CxlRejResponseTo, ExecType, GroupSpec, HandlInst, MsgType,
    OrdRejReason, OrdStatus, OrdType, SessionRejectReason, Side, TimeInForce, tags,
};
use super::validate::{Header, SessionReject, parse_utc_timestamp, validate_body};

/// Receives validated application messages, in sequence order.
pub trait Application {
    /// The session just became active. A good place to send queued orders.
    fn on_logon<O: Outbox>(&mut self, _send: &mut Sender<'_, O>) {}

    /// One application message. `Err` is answered with a BusinessMessageReject
    /// (35=j); the message's sequence number is consumed either way.
    fn on_message<'buf, O: Outbox>(&mut self, msg: &Inbound<'_, 'buf>, send: &mut Sender<'_, O>)
        -> Result<(), BusinessReject<'buf>>;

    /// Called on every loop iteration while the session is active, for
    /// unsolicited messages.
    fn on_timer<O: Outbox>(&mut self, _send: &mut Sender<'_, O>) {}
}

/// Rejects every application message as unsupported.
pub struct NoApplication;

impl Application for NoApplication {
    fn on_message<'buf, O: Outbox>(&mut self, msg: &Inbound<'_, 'buf>, _send: &mut Sender<'_, O>)
        -> Result<(), BusinessReject<'buf>>
    {
        Err(BusinessReject::unsupported(msg))
    }
}

/// What the application gets for each message.
pub struct Inbound<'a, 'buf> {
    pub header: &'a Header<'buf>,
    /// The whole message, for fields the view doesn't carry.
    pub message: &'a Message<'buf>,
    pub view: AppMessage<'buf>,
}

/// Everything a BusinessMessageReject (35=j) needs besides RefSeqNum (45) and
/// RefMsgType (372), which come from the rejected message.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct BusinessReject<'a> {
    /// BusinessRejectReason (380).
    pub reason: BusinessRejectReason,
    /// BusinessRejectRefID (379): the rejected message's business ID, e.g. its ClOrdID.
    pub ref_id: Option<&'a str>,
    /// Text (58).
    pub text: &'a str,
}

impl<'a> BusinessReject<'a> {
    pub fn new(reason: BusinessRejectReason, ref_id: Option<&'a str>, text: &'a str) -> Self {
        Self { reason, ref_id, text }
    }

    /// UnsupportedMessageType (380=3) for `msg`.
    pub fn unsupported(msg: &Inbound<'_, 'a>) -> Self {
        Self::new(BusinessRejectReason::UnsupportedMessageType, msg.view.business_id(), "Unsupported message type")
    }
}

/// Why `decode` refused a message.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AppReject<'buf> {
    /// Schema or format problem: Reject (35=3).
    Session(SessionReject<'buf>),
    /// Business rule: BusinessMessageReject (35=j).
    Business(BusinessReject<'buf>),
}

impl<'buf> From<SessionReject<'buf>> for AppReject<'buf> {
    fn from(r: SessionReject<'buf>) -> Self {
        AppReject::Session(r)
    }
}

/// Instrument component, as far as the dictionary carries it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Instrument<'buf> {
    pub symbol: &'buf str,
    pub security_id: Option<&'buf str>,
    pub security_id_source: Option<&'buf str>,
    pub security_exchange: Option<&'buf str>,
}

/// The order itself: NewOrderSingle (D), and the new terms of an
/// OrderCancelReplaceRequest (G).
#[derive(Debug, Clone, Copy)]
pub struct Order<'buf> {
    pub cl_ord_id: &'buf str,
    /// NoPartyIDs (453): PartyID, PartyIDSource, PartyRole, nested NoPartySubIDs.
    pub parties: Option<Group<'buf>>,
    pub account: Option<&'buf str>,
    pub handl_inst: Option<HandlInst>,
    pub exec_inst: Option<&'buf str>,
    pub min_qty: Option<Decimal>,
    pub max_floor: Option<Decimal>,
    pub instrument: Instrument<'buf>,
    pub side: Side,
    /// Nanoseconds since the Unix epoch.
    pub transact_time: i64,
    pub order_qty: Decimal,
    pub ord_type: OrdType,
    /// Present for limit-priced order types.
    pub price: Option<Decimal>,
    /// Present for stop order types.
    pub stop_px: Option<Decimal>,
    pub currency: Option<&'buf str>,
    pub time_in_force: Option<TimeInForce>,
    /// LocalMktDate, YYYYMMDD.
    pub expire_date: Option<&'buf str>,
    pub expire_time: Option<i64>,
    pub text: Option<&'buf str>,
}

/// OrderCancelRequest (F).
#[derive(Debug, Clone, Copy)]
pub struct OrderCancelRequest<'buf> {
    pub orig_cl_ord_id: &'buf str,
    pub order_id: Option<&'buf str>,
    pub cl_ord_id: &'buf str,
    pub account: Option<&'buf str>,
    pub parties: Option<Group<'buf>>,
    pub instrument: Instrument<'buf>,
    pub side: Side,
    pub transact_time: i64,
    pub order_qty: Option<Decimal>,
    pub text: Option<&'buf str>,
}

/// OrderCancelReplaceRequest (G).
#[derive(Debug, Clone, Copy)]
pub struct OrderCancelReplaceRequest<'buf> {
    pub order_id: Option<&'buf str>,
    pub orig_cl_ord_id: &'buf str,
    /// The replacement order; its `cl_ord_id` is the new ClOrdID.
    pub order: Order<'buf>,
}

/// ExecutionReport (8).
#[derive(Debug, Clone, Copy)]
pub struct ExecutionReport<'buf> {
    pub order_id: &'buf str,
    pub cl_ord_id: Option<&'buf str>,
    pub orig_cl_ord_id: Option<&'buf str>,
    pub parties: Option<Group<'buf>>,
    pub exec_id: &'buf str,
    pub exec_type: ExecType,
    pub ord_status: OrdStatus,
    pub ord_rej_reason: Option<OrdRejReason>,
    pub account: Option<&'buf str>,
    pub instrument: Instrument<'buf>,
    pub side: Side,
    pub order_qty: Option<Decimal>,
    pub ord_type: Option<OrdType>,
    pub price: Option<Decimal>,
    pub stop_px: Option<Decimal>,
    pub currency: Option<&'buf str>,
    pub time_in_force: Option<TimeInForce>,
    /// Present on fills (ExecType F).
    pub last_qty: Option<Decimal>,
    pub last_px: Option<Decimal>,
    pub leaves_qty: Decimal,
    pub cum_qty: Decimal,
    pub avg_px: Decimal,
    pub transact_time: Option<i64>,
    pub text: Option<&'buf str>,
}

/// OrderCancelReject (9).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct OrderCancelReject<'buf> {
    pub order_id: &'buf str,
    pub cl_ord_id: &'buf str,
    pub orig_cl_ord_id: &'buf str,
    pub ord_status: OrdStatus,
    pub account: Option<&'buf str>,
    pub transact_time: Option<i64>,
    pub response_to: CxlRejResponseTo,
    pub reason: Option<CxlRejReason>,
    pub text: Option<&'buf str>,
}

/// BusinessMessageReject (j), received from the counterparty.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct BusinessMessageReject<'buf> {
    pub ref_seq_num: Option<u32>,
    pub ref_msg_type: &'buf [u8],
    pub ref_id: Option<&'buf str>,
    pub reason: BusinessRejectReason,
    pub text: Option<&'buf str>,
}

#[derive(Debug, Clone, Copy)]
pub enum AppMessage<'buf> {
    NewOrderSingle(Order<'buf>),
    OrderCancelRequest(OrderCancelRequest<'buf>),
    OrderCancelReplaceRequest(OrderCancelReplaceRequest<'buf>),
    ExecutionReport(ExecutionReport<'buf>),
    OrderCancelReject(OrderCancelReject<'buf>),
    BusinessMessageReject(BusinessMessageReject<'buf>),
}

impl<'buf> AppMessage<'buf> {
    /// The ID a BusinessMessageReject should reference (BusinessRejectRefID).
    pub fn business_id(&self) -> Option<&'buf str> {
        match self {
            AppMessage::NewOrderSingle(o) => Some(o.cl_ord_id),
            AppMessage::OrderCancelRequest(c) => Some(c.cl_ord_id),
            AppMessage::OrderCancelReplaceRequest(r) => Some(r.order.cl_ord_id),
            AppMessage::ExecutionReport(e) => Some(e.exec_id),
            AppMessage::OrderCancelReject(c) => Some(c.cl_ord_id),
            AppMessage::BusinessMessageReject(_) => None,
        }
    }
}

/// Validates the message against its spec and builds the typed view.
pub fn decode<'buf>(msg: &Message<'buf>, header: &Header<'buf>) -> Result<AppMessage<'buf>, AppReject<'buf>> {
    let spec = validate_body(msg, header)?;
    if spec.admin {
        return Err(AppReject::Session(SessionReject {
            ref_seq_num: Some(header.msg_seq_num),
            ref_tag: Some(tags::MSG_TYPE),
            ref_msg_type: Some(header.msg_type),
            reason: SessionRejectReason::InvalidMsgType,
            text: "Not an application message",
        }))
    }
    let f = Fields { msg, header };
    let msg_type = MsgType::from_bytes(header.msg_type).expect("validate_body accepted the MsgType");
    Ok(match msg_type {
        MsgType::NewOrderSingle => AppMessage::NewOrderSingle(f.order(&spec::NEW_ORDER_SINGLE_NO_PARTY_IDS)?),

        MsgType::OrderCancelRequest => AppMessage::OrderCancelRequest(OrderCancelRequest {
            orig_cl_ord_id: f.required_str(tags::ORIG_CL_ORD_ID),
            order_id: f.str(tags::ORDER_ID),
            cl_ord_id: f.required_str(tags::CL_ORD_ID),
            account: f.str(tags::ACCOUNT),
            parties: f.group(&spec::ORDER_CANCEL_REQUEST_NO_PARTY_IDS),
            instrument: f.instrument(),
            side: f.required_enum(tags::SIDE, Side::from_bytes),
            transact_time: f.required_time(tags::TRANSACT_TIME),
            order_qty: f.decimal(tags::ORDER_QTY)?,
            text: f.str(tags::TEXT),
        }),

        MsgType::OrderCancelReplaceRequest => AppMessage::OrderCancelReplaceRequest(OrderCancelReplaceRequest {
            order_id: f.str(tags::ORDER_ID),
            orig_cl_ord_id: f.required_str(tags::ORIG_CL_ORD_ID),
            order: f.order(&spec::ORDER_CANCEL_REPLACE_REQUEST_NO_PARTY_IDS)?,
        }),

        MsgType::ExecutionReport => {
            let report = ExecutionReport {
                order_id: f.required_str(tags::ORDER_ID),
                cl_ord_id: f.str(tags::CL_ORD_ID),
                orig_cl_ord_id: f.str(tags::ORIG_CL_ORD_ID),
                parties: f.group(&spec::EXECUTION_REPORT_NO_PARTY_IDS),
                exec_id: f.required_str(tags::EXEC_ID),
                exec_type: f.required_enum(tags::EXEC_TYPE, ExecType::from_bytes),
                ord_status: f.required_enum(tags::ORD_STATUS, OrdStatus::from_bytes),
                ord_rej_reason: f.enum_value(tags::ORD_REJ_REASON, OrdRejReason::from_bytes),
                account: f.str(tags::ACCOUNT),
                instrument: f.instrument(),
                side: f.required_enum(tags::SIDE, Side::from_bytes),
                order_qty: f.decimal(tags::ORDER_QTY)?,
                ord_type: f.enum_value(tags::ORD_TYPE, OrdType::from_bytes),
                price: f.decimal(tags::PRICE)?,
                stop_px: f.decimal(tags::STOP_PX)?,
                currency: f.str(tags::CURRENCY),
                time_in_force: f.enum_value(tags::TIME_IN_FORCE, TimeInForce::from_bytes),
                last_qty: f.decimal(tags::LAST_QTY)?,
                last_px: f.decimal(tags::LAST_PX)?,
                leaves_qty: f.required_decimal(tags::LEAVES_QTY)?,
                cum_qty: f.required_decimal(tags::CUM_QTY)?,
                avg_px: f.required_decimal(tags::AVG_PX)?,
                transact_time: f.time(tags::TRANSACT_TIME),
                text: f.str(tags::TEXT),
            };
            if report.exec_type == ExecType::Trade {
                f.conditionally_required(report.last_qty, Some(report.exec_id), "LastQty(32) required on a fill")?;
                f.conditionally_required(report.last_px, Some(report.exec_id), "LastPx(31) required on a fill")?;
            }
            AppMessage::ExecutionReport(report)
        }

        MsgType::OrderCancelReject => AppMessage::OrderCancelReject(OrderCancelReject {
            order_id: f.required_str(tags::ORDER_ID),
            cl_ord_id: f.required_str(tags::CL_ORD_ID),
            orig_cl_ord_id: f.required_str(tags::ORIG_CL_ORD_ID),
            ord_status: f.required_enum(tags::ORD_STATUS, OrdStatus::from_bytes),
            account: f.str(tags::ACCOUNT),
            transact_time: f.time(tags::TRANSACT_TIME),
            response_to: f.required_enum(tags::CXL_REJ_RESPONSE_TO, CxlRejResponseTo::from_bytes),
            reason: f.enum_value(tags::CXL_REJ_REASON, CxlRejReason::from_bytes),
            text: f.str(tags::TEXT),
        }),

        MsgType::BusinessMessageReject => AppMessage::BusinessMessageReject(BusinessMessageReject {
            ref_seq_num: f.seq(tags::REF_SEQ_NUM)?,
            ref_msg_type: msg.fields.get_slice(tags::REF_MSG_TYPE, msg.raw).expect("required tag checked by the schema"),
            ref_id: f.str(tags::BUSINESS_REJECT_REF_ID),
            reason: f.required_enum(tags::BUSINESS_REJECT_REASON, BusinessRejectReason::from_bytes),
            text: f.str(tags::TEXT),
        }),

        _ => unreachable!("admin MsgTypes returned above"),
    })
}

/// Field access for `decode`. The schema check already guaranteed presence of
/// required tags, value formats and enum values, so only range problems and
/// business rules can fail here.
struct Fields<'a, 'buf> {
    msg: &'a Message<'buf>,
    header: &'a Header<'buf>,
}

impl<'buf> Fields<'_, 'buf> {
    fn str(&self, tag: u16) -> Option<&'buf str> {
        self.msg.get_str(tag)
    }

    fn required_str(&self, tag: u16) -> &'buf str {
        self.str(tag).expect("required tag checked by the schema")
    }

    fn enum_value<T>(&self, tag: u16, from_bytes: fn(&[u8]) -> Option<T>) -> Option<T> {
        self.msg.fields.get_slice(tag, self.msg.raw).map(|v| from_bytes(v).expect("enum checked by the schema"))
    }

    fn required_enum<T>(&self, tag: u16, from_bytes: fn(&[u8]) -> Option<T>) -> T {
        self.enum_value(tag, from_bytes).expect("required tag checked by the schema")
    }

    fn time(&self, tag: u16) -> Option<i64> {
        self.msg.fields.get_slice(tag, self.msg.raw).map(|v| parse_utc_timestamp(v).expect("format checked by the schema"))
    }

    fn required_time(&self, tag: u16) -> i64 {
        self.time(tag).expect("required tag checked by the schema")
    }

    fn out_of_range(&self, tag: u16) -> SessionReject<'buf> {
        SessionReject {
            ref_seq_num: Some(self.header.msg_seq_num),
            ref_tag: Some(tag),
            ref_msg_type: Some(self.header.msg_type),
            reason: SessionRejectReason::ValueIsIncorrect,
            text: "Value out of range",
        }
    }

    fn decimal(&self, tag: u16) -> Result<Option<Decimal>, SessionReject<'buf>> {
        match self.msg.fields.get_slice(tag, self.msg.raw) {
            None => Ok(None),
            Some(v) => decode::to_decimal(v).map(Some).ok_or(self.out_of_range(tag)),
        }
    }

    fn required_decimal(&self, tag: u16) -> Result<Decimal, SessionReject<'buf>> {
        self.decimal(tag).map(|v| v.expect("required tag checked by the schema"))
    }

    fn seq(&self, tag: u16) -> Result<Option<u32>, SessionReject<'buf>> {
        match self.msg.fields.get_slice(tag, self.msg.raw) {
            None => Ok(None),
            Some(v) => decode::to_u32(v).map(Some).ok_or(self.out_of_range(tag)),
        }
    }

    fn group(&self, spec: &GroupSpec) -> Option<Group<'buf>> {
        self.msg.group(spec).expect("group checked by the schema")
    }

    fn instrument(&self) -> Instrument<'buf> {
        Instrument {
            symbol: self.required_str(tags::SYMBOL),
            security_id: self.str(tags::SECURITY_ID),
            security_id_source: self.str(tags::SECURITY_ID_SOURCE),
            security_exchange: self.str(tags::SECURITY_EXCHANGE),
        }
    }

    fn conditionally_required<T>(&self, value: Option<T>, ref_id: Option<&'buf str>, text: &'static str) -> Result<T, AppReject<'buf>> {
        value.ok_or(AppReject::Business(BusinessReject::new(BusinessRejectReason::ConditionallyRequiredFieldMissing, ref_id, text)))
    }

    /// D and G share the order fields.
    fn order(&self, parties: &GroupSpec) -> Result<Order<'buf>, AppReject<'buf>> {
        let order = Order {
            cl_ord_id: self.required_str(tags::CL_ORD_ID),
            parties: self.group(parties),
            account: self.str(tags::ACCOUNT),
            handl_inst: self.enum_value(tags::HANDL_INST, HandlInst::from_bytes),
            exec_inst: self.str(tags::EXEC_INST),
            min_qty: self.decimal(tags::MIN_QTY)?,
            max_floor: self.decimal(tags::MAX_FLOOR)?,
            instrument: self.instrument(),
            side: self.required_enum(tags::SIDE, Side::from_bytes),
            transact_time: self.required_time(tags::TRANSACT_TIME),
            order_qty: self.required_decimal(tags::ORDER_QTY)?,
            ord_type: self.required_enum(tags::ORD_TYPE, OrdType::from_bytes),
            price: self.decimal(tags::PRICE)?,
            stop_px: self.decimal(tags::STOP_PX)?,
            currency: self.str(tags::CURRENCY),
            time_in_force: self.enum_value(tags::TIME_IN_FORCE, TimeInForce::from_bytes),
            expire_date: self.str(tags::EXPIRE_DATE),
            expire_time: self.time(tags::EXPIRE_TIME),
            text: self.str(tags::TEXT),
        };
        let id = Some(order.cl_ord_id);
        if matches!(order.ord_type, OrdType::Limit | OrdType::StopLimit | OrdType::LimitOrBetter | OrdType::LimitWithOrWithout) {
            self.conditionally_required(order.price, id, "Price(44) required for this OrdType")?;
        }
        if matches!(order.ord_type, OrdType::Stop | OrdType::StopLimit) {
            self.conditionally_required(order.stop_px, id, "StopPx(99) required for this OrdType")?;
        }
        if order.time_in_force == Some(TimeInForce::GoodTillDate) && order.expire_date.is_none() && order.expire_time.is_none() {
            self.conditionally_required(None::<()>, id, "ExpireDate(432) or ExpireTime(126) required for GTD")?;
        }
        Ok(order)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{ParseOutcome, Parser};
    use crate::fix::validate::{HeaderRules, validate_header};

    const SECOND: i64 = 1_000_000_000;
    const NOW: i64 = 1_792_947_600 * SECOND; // 2026-10-25 17:00:00 UTC
    const RULES: HeaderRules = HeaderRules { our_comp_id: b"GATEWAY", their_comp_id: b"CLIENT", max_sending_time_skew_ns: 120 * SECOND };
    const ORDER: &str = "11=ord-1|453=1|448=TRADER7|447=D|452=11|1=ACC|21=1|55=MSFT|54=1|60=20261025-17:00:00.000|38=100|40=2|44=412.50|59=0|";

    fn frame(msg_type: &str, fields: &str) -> Vec<u8> {
        let body = format!("35={msg_type}|49=CLIENT|56=GATEWAY|34=7|52=20261025-17:00:00.000|{fields}").replace('|', "\x01");
        let mut out = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
        let sum = out.iter().fold(0u32, |s, &b| s + b as u32) % 256;
        out.extend(format!("10={:03}\x01", sum).bytes());
        out
    }

    fn with_view<R>(msg_type: &str, fields: &str, check: impl FnOnce(Result<AppMessage, AppReject>) -> R) -> R {
        let raw = frame(msg_type, fields);
        let mut parser = Box::new(Parser::new());
        let ParseOutcome::Message(msg) = parser.parse_frame(&raw) else { panic!("didn't parse") };
        let header = validate_header(&msg, &RULES, NOW).expect("header ok");
        check(decode(&msg, &header))
    }

    fn business_reject(msg_type: &str, fields: &str) -> (BusinessRejectReason, Option<String>, String) {
        with_view(msg_type, fields, |v| match v {
            Err(AppReject::Business(b)) => (b.reason, b.ref_id.map(str::to_string), b.text.to_string()),
            other => panic!("expected a business reject, got {other:?}"),
        })
    }

    fn session_reject(msg_type: &str, fields: &str) -> (Option<u16>, SessionRejectReason) {
        with_view(msg_type, fields, |v| match v {
            Err(AppReject::Session(r)) => (r.ref_tag, r.reason),
            other => panic!("expected a session reject, got {other:?}"),
        })
    }

    #[test]
    fn new_order_single() {
        with_view("D", ORDER, |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            assert_eq!(o.cl_ord_id, "ord-1");
            assert_eq!((o.account, o.handl_inst), (Some("ACC"), Some(HandlInst::AutomatedPrivate)));
            assert_eq!(o.instrument, Instrument { symbol: "MSFT", security_id: None, security_id_source: None, security_exchange: None });
            assert_eq!((o.side, o.ord_type, o.time_in_force), (Side::Buy, OrdType::Limit, Some(TimeInForce::Day)));
            assert_eq!((o.order_qty, o.price, o.stop_px), (Decimal::new(100, 0), Some(Decimal::new(41250, 2)), None));
            assert_eq!(o.transact_time, NOW);
            let parties: Vec<_> = o.parties.unwrap().entries().map(|e| (e.get_str(448), e.get_str(452))).collect();
            assert_eq!(parties, [(Some("TRADER7"), Some("11"))]);
        });
        // Market order: no price needed.
        with_view("D", "11=m|55=MSFT|54=2|60=20261025-17:00:00|38=5|40=1|", |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            assert_eq!((o.side, o.ord_type, o.price, o.parties.is_none()), (Side::Sell, OrdType::Market, None, true));
        });
    }

    #[test]
    fn nested_party_sub_ids() {
        with_view("D", "11=n|453=1|448=P|447=D|452=3|802=2|523=desk|803=4|523=floor|803=10|55=X|54=1|60=20261025-17:00:00|38=1|40=1|", |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            let party = o.parties.unwrap().entries().next().unwrap();
            let subs = party.group(&spec::NEW_ORDER_SINGLE_NO_PARTY_IDS_NO_PARTY_SUB_IDS).unwrap().unwrap();
            let ids: Vec<_> = subs.entries().map(|e| e.get_str(523)).collect();
            assert_eq!(ids, [Some("desk"), Some("floor")]);
        });
    }

    #[test]
    fn conditionally_required_order_fields() {
        let (reason, id, text) = business_reject("D", "11=l|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=2|");
        assert_eq!((reason, id.as_deref()), (BusinessRejectReason::ConditionallyRequiredFieldMissing, Some("l")));
        assert!(text.contains("Price(44)"), "{text}");
        let (_, _, text) = business_reject("D", "11=s|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=4|44=10|");
        assert!(text.contains("StopPx(99)"), "{text}");
        let (_, _, text) = business_reject("D", "11=g|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=1|59=6|");
        assert!(text.contains("GTD"), "{text}");
        with_view("D", "11=g|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=1|59=6|432=20261231|", |v| assert!(v.is_ok()));
        // Same rules on a replace.
        let (_, id, text) = business_reject("G", "41=ord-1|11=ord-2|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=2|");
        assert_eq!(id.as_deref(), Some("ord-2"));
        assert!(text.contains("Price(44)"), "{text}");
    }

    #[test]
    fn schema_problems_are_session_rejects() {
        // Missing required Side.
        assert_eq!(session_reject("D", "11=x|55=MSFT|60=20261025-17:00:00|38=5|40=1|"), (Some(54), SessionRejectReason::RequiredTagMissing));
        // Unknown Side value.
        assert_eq!(session_reject("D", "11=x|55=MSFT|54=Z|60=20261025-17:00:00|38=5|40=1|"), (Some(54), SessionRejectReason::ValueIsIncorrect));
        // Malformed price.
        assert_eq!(session_reject("D", "11=x|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=2|44=1.2.3|"), (Some(44), SessionRejectReason::IncorrectDataFormatForValue));
        // Well formed but past i64.
        assert_eq!(session_reject("D", "11=x|55=MSFT|54=1|60=20261025-17:00:00|38=99999999999999999999|40=1|"), (Some(38), SessionRejectReason::ValueIsIncorrect));
        // Order field on a cancel.
        assert_eq!(session_reject("F", "41=a|11=b|55=MSFT|54=1|60=20261025-17:00:00|40=1|"), (Some(40), SessionRejectReason::TagNotDefinedForThisMessageType));
    }

    #[test]
    fn cancel_and_replace() {
        with_view("F", "41=ord-1|37=O9|11=ord-2|55=MSFT|54=1|60=20261025-17:00:00|38=100|", |v| {
            let AppMessage::OrderCancelRequest(c) = v.unwrap() else { panic!() };
            assert_eq!((c.orig_cl_ord_id, c.order_id, c.cl_ord_id), ("ord-1", Some("O9"), "ord-2"));
            assert_eq!(c.order_qty, Some(Decimal::new(100, 0)));
        });
        with_view("G", "37=O9|41=ord-1|11=ord-2|55=MSFT|54=1|60=20261025-17:00:00|38=200|40=2|44=413|", |v| {
            let AppMessage::OrderCancelReplaceRequest(r) = v.unwrap() else { panic!() };
            assert_eq!((r.order_id, r.orig_cl_ord_id, r.order.cl_ord_id), (Some("O9"), "ord-1", "ord-2"));
            assert_eq!((r.order.order_qty, r.order.price), (Decimal::new(200, 0), Some(Decimal::new(413, 0))));
        });
    }

    #[test]
    fn execution_report() {
        let fill = "37=O1|11=ord-1|17=E2|150=F|39=1|55=MSFT|54=1|38=100|32=40|31=412.5|151=60|14=40|6=412.5|60=20261025-17:00:00|";
        with_view("8", fill, |v| {
            let AppMessage::ExecutionReport(e) = v.unwrap() else { panic!() };
            assert_eq!((e.order_id, e.exec_id, e.exec_type, e.ord_status), ("O1", "E2", ExecType::Trade, OrdStatus::PartiallyFilled));
            assert_eq!((e.last_qty, e.last_px), (Some(Decimal::new(40, 0)), Some(Decimal::new(4125, 1))));
            assert_eq!((e.leaves_qty, e.cum_qty), (Decimal::new(60, 0), Decimal::new(40, 0)));
        });
        let (reason, id, text) = business_reject("8", "37=O1|17=E3|150=F|39=1|55=MSFT|54=1|31=1|151=60|14=40|6=1|");
        assert_eq!((reason, id.as_deref()), (BusinessRejectReason::ConditionallyRequiredFieldMissing, Some("E3")));
        assert!(text.contains("LastQty(32)"), "{text}");
        with_view("8", "37=O1|17=E4|150=8|39=8|103=1|55=MSFT|54=1|151=0|14=0|6=0|", |v| {
            let AppMessage::ExecutionReport(e) = v.unwrap() else { panic!() };
            assert_eq!(e.ord_rej_reason, Some(OrdRejReason::UnknownSymbol));
        });
    }

    #[test]
    fn cancel_reject_and_business_reject() {
        with_view("9", "37=O1|11=ord-2|41=ord-1|39=0|434=1|102=1|58=no such order|", |v| {
            let AppMessage::OrderCancelReject(c) = v.unwrap() else { panic!() };
            assert_eq!((c.response_to, c.reason, c.text), (CxlRejResponseTo::OrderCancelRequest, Some(CxlRejReason::UnknownOrder), Some("no such order")));
        });
        with_view("j", "45=3|372=D|379=ord-1|380=3|58=nope|", |v| {
            let AppMessage::BusinessMessageReject(b) = v.unwrap() else { panic!() };
            assert_eq!(b, BusinessMessageReject {
                ref_seq_num: Some(3),
                ref_msg_type: b"D",
                ref_id: Some("ord-1"),
                reason: BusinessRejectReason::UnsupportedMessageType,
                text: Some("nope"),
            });
        });
        assert_eq!(session_reject("j", "45=3|380=3|"), (Some(372), SessionRejectReason::RequiredTagMissing));
    }

    #[test]
    fn admin_messages_are_not_app_messages() {
        assert_eq!(session_reject("0", ""), (Some(35), SessionRejectReason::InvalidMsgType));
    }
}
