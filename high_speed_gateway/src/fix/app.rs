//! Application layer: the `Application` trait the session hands decoded
//! application messages to, and the rejects it can answer with.
//!
//! The message views and their `decode` live in `messages/`. Schema problems
//! are session-level Rejects (35=3); rules the schema can't express (Price on a
//! limit order, LastQty / LastPx on a fill, ...) are BusinessMessageRejects
//! (35=j), as are the application's own refusals.

use super::message::Message;
use super::messages::AppMessage;
use super::session::{Outbox, Sender};
use super::spec::BusinessRejectReason;
use super::validate::{Header, SessionReject};

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
    /// A lazy view of `message`: fields are read when its accessors are called.
    pub view: AppMessage<'a, 'buf>,
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
