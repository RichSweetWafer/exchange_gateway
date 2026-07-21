use std::time::{Duration, Instant};


pub enum SessionState {
    Disconnected,
    LogonSent,
    Connected,
    LogoutSent
}


pub struct SessionController {
    pub state: SessionState,
    pub sender_comp_id: String, // NB: Keep in mind -- using String == memory allocation
    pub target_comp_id: String, // Same

    // Sequence tracking
    pub next_outbound_seq: u32,
    pub next_inbound_seq: u32,

    // Heartbeat
    pub heartbeat_interval: Duration,
    pub last_sent_time: Instant,
    pub last_received_time: Instant,
    pub test_request_id: u32,
    pub awaiting_test_response: bool,

}


impl SessionController {
    
}