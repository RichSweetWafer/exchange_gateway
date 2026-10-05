//! Lazy views of the order-flow messages (D, F, G, 8, 9, j), one file per
//! message.
//!
//! A view is a borrow of the parsed `Message` and nothing else: every accessor
//! reads its field from the FlatIndex when called. `decode` runs the strict
//! schema check (`validate_body`, which also guarantees decimals fit an i64)
//! and the conditionally required rules the schema can't express, so the
//! accessors never fail and an accepted message behaves exactly as before.
//! Schema problems are session-level Rejects (35=3); conditionally required
//! fields missing are BusinessMessageRejects (35=j 380=5).

use super::app::{AppReject, BusinessReject};
use super::decode::{self, Decimal};
use super::message::{Group, Message};
use super::spec::{BusinessRejectReason, GroupSpec, MsgType, SessionRejectReason, tags};
use super::validate::{Header, SessionReject, parse_utc_timestamp, validate_body};

/// Debug for a view: the values of the listed accessors.
macro_rules! debug_view {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl std::fmt::Debug for $ty<'_, '_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty))$(.field(stringify!($field), &self.$field()))*.finish()
            }
        }
    };
}

mod business_message_reject;
mod execution_report;
mod new_order_single;
mod order_cancel_reject;
mod order_cancel_replace_request;
mod order_cancel_request;

pub use business_message_reject::BusinessMessageReject;
pub use execution_report::ExecutionReport;
pub use new_order_single::Order;
pub use order_cancel_reject::OrderCancelReject;
pub use order_cancel_replace_request::OrderCancelReplaceRequest;
pub use order_cancel_request::OrderCancelRequest;

/// One decoded application message. Every variant is a single borrow of the
/// `Message`, so this is two words.
#[derive(Debug, Clone, Copy)]
pub enum AppMessage<'a, 'buf> {
    NewOrderSingle(Order<'a, 'buf>),
    OrderCancelRequest(OrderCancelRequest<'a, 'buf>),
    OrderCancelReplaceRequest(OrderCancelReplaceRequest<'a, 'buf>),
    ExecutionReport(ExecutionReport<'a, 'buf>),
    OrderCancelReject(OrderCancelReject<'a, 'buf>),
    BusinessMessageReject(BusinessMessageReject<'a, 'buf>),
}

impl<'buf> AppMessage<'_, 'buf> {
    /// The ID a BusinessMessageReject should reference (BusinessRejectRefID).
    pub fn business_id(&self) -> Option<&'buf str> {
        match self {
            AppMessage::NewOrderSingle(o) => Some(o.cl_ord_id()),
            AppMessage::OrderCancelRequest(c) => Some(c.cl_ord_id()),
            AppMessage::OrderCancelReplaceRequest(r) => Some(r.order().cl_ord_id()),
            AppMessage::ExecutionReport(e) => Some(e.exec_id()),
            AppMessage::OrderCancelReject(c) => Some(c.cl_ord_id()),
            AppMessage::BusinessMessageReject(_) => None,
        }
    }
}

/// Validates the message against its spec, checks the rules the schema can't
/// express and wraps it in its view.
pub fn decode<'a, 'buf>(msg: &'a Message<'buf>, header: &Header<'buf>) -> Result<AppMessage<'a, 'buf>, AppReject<'buf>> {
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
    let msg_type = MsgType::from_bytes(header.msg_type).expect("validate_body accepted the MsgType");
    Ok(match msg_type {
        MsgType::NewOrderSingle => AppMessage::NewOrderSingle(Order::decode(msg)?),
        MsgType::OrderCancelRequest => AppMessage::OrderCancelRequest(OrderCancelRequest::decode(msg)),
        MsgType::OrderCancelReplaceRequest => AppMessage::OrderCancelReplaceRequest(OrderCancelReplaceRequest::decode(msg)?),
        MsgType::ExecutionReport => AppMessage::ExecutionReport(ExecutionReport::decode(msg)?),
        MsgType::OrderCancelReject => AppMessage::OrderCancelReject(OrderCancelReject::decode(msg)),
        MsgType::BusinessMessageReject => AppMessage::BusinessMessageReject(BusinessMessageReject::decode(msg, header)?),
        _ => unreachable!("admin MsgTypes returned above"),
    })
}

/// Instrument component, as far as the dictionary carries it.
#[derive(Clone, Copy)]
pub struct Instrument<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'buf> Instrument<'_, 'buf> {
    pub fn symbol(&self) -> &'buf str {
        required_str(self.msg, tags::SYMBOL)
    }

    pub fn security_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::SECURITY_ID)
    }

    pub fn security_id_source(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::SECURITY_ID_SOURCE)
    }

    pub fn security_exchange(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::SECURITY_EXCHANGE)
    }
}

debug_view!(Instrument { symbol, security_id, security_id_source, security_exchange });

// Field access for the views. `decode` already guaranteed presence of required
// tags, value formats, enum values and decimal ranges, so none of these fail.

const CHECKED: &str = "checked by the schema";

fn instrument<'a, 'buf>(msg: &'a Message<'buf>) -> Instrument<'a, 'buf> {
    Instrument { msg }
}

fn required_str<'buf>(msg: &Message<'buf>, tag: u16) -> &'buf str {
    msg.get_str(tag).expect(CHECKED)
}

fn enum_value<T>(msg: &Message, tag: u16, from_bytes: fn(&[u8]) -> Option<T>) -> Option<T> {
    msg.fields.get_slice(tag, msg.raw).map(|v| from_bytes(v).expect(CHECKED))
}

fn required_enum<T>(msg: &Message, tag: u16, from_bytes: fn(&[u8]) -> Option<T>) -> T {
    enum_value(msg, tag, from_bytes).expect(CHECKED)
}

/// Nanoseconds since the Unix epoch.
fn time(msg: &Message, tag: u16) -> Option<i64> {
    msg.fields.get_slice(tag, msg.raw).map(|v| parse_utc_timestamp(v).expect(CHECKED))
}

fn required_time(msg: &Message, tag: u16) -> i64 {
    time(msg, tag).expect(CHECKED)
}

fn decimal(msg: &Message, tag: u16) -> Option<Decimal> {
    msg.fields.get_slice(tag, msg.raw).map(|v| decode::to_decimal(v).expect(CHECKED))
}

fn required_decimal(msg: &Message, tag: u16) -> Decimal {
    decimal(msg, tag).expect(CHECKED)
}

fn group<'buf>(msg: &Message<'buf>, spec: &GroupSpec) -> Option<Group<'buf>> {
    msg.group(spec).expect(CHECKED)
}

/// BusinessMessageReject 380=5 unless `present`.
fn conditionally_required<'buf>(present: bool, ref_id: Option<&'buf str>, text: &'static str) -> Result<(), AppReject<'buf>> {
    match present {
        true => Ok(()),
        false => Err(AppReject::Business(BusinessReject::new(BusinessRejectReason::ConditionallyRequiredFieldMissing, ref_id, text))),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::parser::{ParseOutcome, Parser};
    use crate::fix::spec::{
        self, CxlRejReason, CxlRejResponseTo, ExecType, HandlInst, OrdRejReason, OrdStatus, OrdType, Side, TimeInForce,
    };
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
    fn app_message_is_two_words() {
        // Was 416 bytes when the views copied every field out at decode time.
        assert_eq!(std::mem::size_of::<AppMessage>(), 16);
        assert_eq!(std::mem::size_of::<Order>(), 8);
    }

    #[test]
    fn new_order_single() {
        with_view("D", ORDER, |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            assert_eq!(o.cl_ord_id(), "ord-1");
            assert_eq!((o.account(), o.handl_inst()), (Some("ACC"), Some(HandlInst::AutomatedPrivate)));
            let i = o.instrument();
            assert_eq!((i.symbol(), i.security_id(), i.security_id_source(), i.security_exchange()), ("MSFT", None, None, None));
            assert_eq!((o.side(), o.ord_type(), o.time_in_force()), (Side::Buy, OrdType::Limit, Some(TimeInForce::Day)));
            assert_eq!((o.order_qty(), o.price(), o.stop_px()), (Decimal::new(100, 0), Some(Decimal::new(41250, 2)), None));
            assert_eq!(o.transact_time(), NOW);
            let parties: Vec<_> = o.parties().unwrap().entries().map(|e| (e.get_str(448), e.get_str(452))).collect();
            assert_eq!(parties, [(Some("TRADER7"), Some("11"))]);
            let debug = format!("{o:?}");
            assert!(debug.contains("cl_ord_id: \"ord-1\"") && debug.contains("symbol: \"MSFT\""), "{debug}");
        });
        // Market order: no price needed.
        with_view("D", "11=m|55=MSFT|54=2|60=20261025-17:00:00|38=5|40=1|", |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            assert_eq!((o.side(), o.ord_type(), o.price(), o.parties().is_none()), (Side::Sell, OrdType::Market, None, true));
        });
    }

    #[test]
    fn nested_party_sub_ids() {
        with_view("D", "11=n|453=1|448=P|447=D|452=3|802=2|523=desk|803=4|523=floor|803=10|55=X|54=1|60=20261025-17:00:00|38=1|40=1|", |v| {
            let AppMessage::NewOrderSingle(o) = v.unwrap() else { panic!() };
            let party = o.parties().unwrap().entries().next().unwrap();
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
        with_view("D", "11=g|55=MSFT|54=1|60=20261025-17:00:00|38=5|40=1|59=6|126=20261231-20:00:00|", |v| assert!(v.is_ok()));
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
            assert_eq!((c.orig_cl_ord_id(), c.order_id(), c.cl_ord_id()), ("ord-1", Some("O9"), "ord-2"));
            assert_eq!(c.order_qty(), Some(Decimal::new(100, 0)));
        });
        with_view("G", "37=O9|41=ord-1|11=ord-2|453=1|448=P|447=D|452=3|55=MSFT|54=1|60=20261025-17:00:00|38=200|40=2|44=413|", |v| {
            let AppMessage::OrderCancelReplaceRequest(r) = v.unwrap() else { panic!() };
            let o = r.order();
            assert_eq!((r.order_id(), r.orig_cl_ord_id(), o.cl_ord_id()), (Some("O9"), "ord-1", "ord-2"));
            assert_eq!((o.order_qty(), o.price()), (Decimal::new(200, 0), Some(Decimal::new(413, 0))));
            // The replace's own NoPartyIDs spec.
            assert_eq!(o.parties().unwrap().entries().next().unwrap().get_str(448), Some("P"));
        });
    }

    #[test]
    fn execution_report() {
        let fill = "37=O1|11=ord-1|17=E2|150=F|39=1|55=MSFT|54=1|38=100|32=40|31=412.5|151=60|14=40|6=412.5|60=20261025-17:00:00|";
        with_view("8", fill, |v| {
            let AppMessage::ExecutionReport(e) = v.unwrap() else { panic!() };
            assert_eq!((e.order_id(), e.exec_id(), e.exec_type(), e.ord_status()), ("O1", "E2", ExecType::Trade, OrdStatus::PartiallyFilled));
            assert_eq!((e.last_qty(), e.last_px()), (Some(Decimal::new(40, 0)), Some(Decimal::new(4125, 1))));
            assert_eq!((e.leaves_qty(), e.cum_qty()), (Decimal::new(60, 0), Decimal::new(40, 0)));
            assert_eq!(e.transact_time(), Some(NOW));
        });
        let (reason, id, text) = business_reject("8", "37=O1|17=E3|150=F|39=1|55=MSFT|54=1|31=1|151=60|14=40|6=1|");
        assert_eq!((reason, id.as_deref()), (BusinessRejectReason::ConditionallyRequiredFieldMissing, Some("E3")));
        assert!(text.contains("LastQty(32)"), "{text}");
        let (_, _, text) = business_reject("8", "37=O1|17=E3|150=F|39=1|55=MSFT|54=1|32=1|151=60|14=40|6=1|");
        assert!(text.contains("LastPx(31)"), "{text}");
        with_view("8", "37=O1|17=E4|150=8|39=8|103=1|55=MSFT|54=1|151=0|14=0|6=0|", |v| {
            let AppMessage::ExecutionReport(e) = v.unwrap() else { panic!() };
            assert_eq!(e.ord_rej_reason(), Some(OrdRejReason::UnknownSymbol));
        });
    }

    #[test]
    fn cancel_reject_and_business_reject() {
        with_view("9", "37=O1|11=ord-2|41=ord-1|39=0|434=1|102=1|58=no such order|", |v| {
            let AppMessage::OrderCancelReject(c) = v.unwrap() else { panic!() };
            assert_eq!((c.response_to(), c.reason(), c.text()), (CxlRejResponseTo::OrderCancelRequest, Some(CxlRejReason::UnknownOrder), Some("no such order")));
        });
        with_view("j", "45=3|372=D|379=ord-1|380=3|58=nope|", |v| {
            let AppMessage::BusinessMessageReject(b) = v.unwrap() else { panic!() };
            assert_eq!((b.ref_seq_num(), b.ref_msg_type(), b.ref_id()), (Some(3), &b"D"[..], Some("ord-1")));
            assert_eq!((b.reason(), b.text()), (BusinessRejectReason::UnsupportedMessageType, Some("nope")));
        });
        assert_eq!(session_reject("j", "45=3|380=3|"), (Some(372), SessionRejectReason::RequiredTagMissing));
        // RefSeqNum past u32 is caught at decode time, not by the accessor.
        assert_eq!(session_reject("j", "45=4294967296|372=D|380=3|"), (Some(45), SessionRejectReason::ValueIsIncorrect));
    }

    #[test]
    fn admin_messages_are_not_app_messages() {
        assert_eq!(session_reject("0", ""), (Some(35), SessionRejectReason::InvalidMsgType));
    }
}
