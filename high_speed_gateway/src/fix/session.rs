
use super::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    
    AwaitingLogon,
    
    Established,

    Recovering,
    
    LogoutPending,
    
    Closed,
}


const MSG_SEQ_NUM: u16 = 34;

pub enum SessionAction {
    SendLogon,
    
    SendHeartbeat,
    
    SendTestRequest,
    
    SendLogout,
    
    SendResendRequest {begin: u32, end: u32},
    
    SendReject {
        seq_num: Option<u32>,
        tag: Option<u16>,
        reason: SessionRejectReason,
    },
    
    Close,

    None
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRejectReason {
    RequiredTagMissing,
    InvalidTagValue,
    IncorrectDataFormat,
    ValueOutOfRange,
    CompIdProblem,
    SendingTimeAccuracyProblem,
    InvalidMsgType,
    Other,
}

pub enum SequenceValidation {
    
    // MsgSeqNum is exactly what we expect
    Accept,

    // Duplicate/retransmission
    Old,

    // Ahead, gap recovery
    Gap {
        expected: u32,
        received: u32
    },

    Invalid
}

pub enum SessionError {
    InvalidPossDupFlag,
    MissingOrigSendingTime,
}

pub struct HeartbeatState {
    pub heartbeat_interval: Duration,
    pub last_rx: Instant,
    pub last_tx: Instant,
    pub test_request_pending: bool,
}


#[derive(Debug)]
pub struct Session {
    state: SessionState,

    next_in_seq: u32,
    next_out_seq: u32,
    
    heartbeat_interval: Duration,
    heartbeat_state: HeartbeatState,
    logon_state: LogonState,

    //
}


impl Session {

    #[inline(always)]
    pub fn on_message(&mut self, msg: &'a Message) -> SessionAction {

        match self.validate_seq(msg) {
            SequenceValidation::Accept => self.next_in_seq += 1,
            SequenceValidation::Old => {
                // check PossDupFlag(43)
            }
            SequenceValidation::Gap { expected, received } => {
                // Resend/recovery
            },
            SequenceValidation::Invalid => {
                // Reject
            }
        }

        match msg.get_msg_type() {
            None => { /* error case */ },
            Some([b'A']) =>
        }
        SessionAction::None
    }

    #[inline(always)]
    fn dispatch(&mut self, msg: &'a Message) {

    }

    #[inline(always)]
    fn validate_seq(&mut self, msg: &'a Message) -> SequenceValidation {
        // sequence number validation
        let received = match msg.get_int(MSG_SEQ_NUM) {
            Some(Ok(seq)) => seq as u32,
            _ => return SequenceValidation::Invalid,
        };

        match received.cmp(&self.next_in_seq) {
            std::cmp::Ordering::Equal => SequenceValidation::Accept,
            std::cmp::Ordering::Greater => SequenceValidation::Gap { expected: self.next_in_seq, received },
            std::cmp::Ordering::Less => SequenceValidation::Old,
        }
    }

    #[inline(always)]
    fn validate_poss_dup(&mut self, msg: &'a Message) -> Result<bool, SessionError> {
        match msg.get_boolean(43) {
            Some(true) => {
                if msg.get_str(122).is_none() {
                    return Err(SessionError::MissingOrigSendingTime)
                }
                self.handle_poss_dup()
            },
            Some(false) => self.handle_sequence_error(),
            _ => self.handle_protocol_error()
        }
    }

}