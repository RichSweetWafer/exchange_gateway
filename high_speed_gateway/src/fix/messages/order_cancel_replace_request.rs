//! OrderCancelReplaceRequest (G).

use super::{Order, required_str};
use crate::fix::app::AppReject;
use crate::fix::message::Message;
use crate::fix::spec::tags;

#[derive(Clone, Copy)]
pub struct OrderCancelReplaceRequest<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> OrderCancelReplaceRequest<'a, 'buf> {
    /// The new terms follow the same rules as a NewOrderSingle.
    pub(super) fn decode(msg: &'a Message<'buf>) -> Result<Self, AppReject<'buf>> {
        Order::decode(msg)?;
        Ok(Self { msg })
    }

    pub fn order_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::ORDER_ID)
    }

    pub fn orig_cl_ord_id(&self) -> &'buf str {
        required_str(self.msg, tags::ORIG_CL_ORD_ID)
    }

    /// The replacement order; its `cl_ord_id` is the new ClOrdID.
    pub fn order(&self) -> Order<'a, 'buf> {
        Order::checked(self.msg)
    }
}

debug_view!(OrderCancelReplaceRequest { order_id, orig_cl_ord_id, order });
