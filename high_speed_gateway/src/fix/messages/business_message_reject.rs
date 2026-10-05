//! BusinessMessageReject (j), received from the counterparty.

use super::{CHECKED, required_enum};
use crate::fix::app::AppReject;
use crate::fix::decode;
use crate::fix::message::Message;
use crate::fix::spec::{BusinessRejectReason, SessionRejectReason, tags};
use crate::fix::validate::{Header, SessionReject};

#[derive(Clone, Copy)]
pub struct BusinessMessageReject<'a, 'buf> {
    msg: &'a Message<'buf>,
}

impl<'a, 'buf> BusinessMessageReject<'a, 'buf> {
    /// Rejects (35=3) a RefSeqNum past the u32 range; the schema only checks
    /// it's digits.
    pub(super) fn decode(msg: &'a Message<'buf>, header: &Header<'buf>) -> Result<Self, AppReject<'buf>> {
        if let Some(v) = msg.fields.get_slice(tags::REF_SEQ_NUM, msg.raw) && decode::to_u32(v).is_none() {
            return Err(AppReject::Session(SessionReject {
                ref_seq_num: Some(header.msg_seq_num),
                ref_tag: Some(tags::REF_SEQ_NUM),
                ref_msg_type: Some(header.msg_type),
                reason: SessionRejectReason::ValueIsIncorrect,
                text: "Value out of range",
            }))
        }
        Ok(Self { msg })
    }

    pub fn ref_seq_num(&self) -> Option<u32> {
        self.msg.fields.get_slice(tags::REF_SEQ_NUM, self.msg.raw).map(|v| decode::to_u32(v).expect(CHECKED))
    }

    pub fn ref_msg_type(&self) -> &'buf [u8] {
        self.msg.fields.get_slice(tags::REF_MSG_TYPE, self.msg.raw).expect(CHECKED)
    }

    /// BusinessRejectRefID (379).
    pub fn ref_id(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::BUSINESS_REJECT_REF_ID)
    }

    pub fn reason(&self) -> BusinessRejectReason {
        required_enum(self.msg, tags::BUSINESS_REJECT_REASON, BusinessRejectReason::from_bytes)
    }

    pub fn text(&self) -> Option<&'buf str> {
        self.msg.get_str(tags::TEXT)
    }
}

debug_view!(BusinessMessageReject { ref_seq_num, ref_msg_type, ref_id, reason, text });
