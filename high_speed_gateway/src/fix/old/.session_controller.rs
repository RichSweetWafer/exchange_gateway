use std::time::{Duration, Instant};
use fix::views::{LogonView, LogoutView, Heartbeat};

pub enum SessionState {
    Disconnected,
    LogonSent,
    Connected,
    LogoutSent
}


pub struct SessionController {
    pub state: SessionState,
    pub sender_comp_id: [u8; 512],
    pub target_comp_id: [u8; 512],

    // Sequence tracking
    pub next_outbound_seq: SeqNum,
    pub next_inbound_seq: SeqNum,

    // Heartbeat
    pub heartbeat_interval: Duration,
    pub last_sent_time: Instant,
    pub last_received_time: Instant,
    pub test_request_id: u32,
    pub awaiting_test_response: bool,

}


impl SessionController {
    
    pub fn 
}