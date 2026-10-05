//! One FIX connection: Transport -> RecvBuffer -> Framer -> Parser -> Session -> Application,
//! with outbound messages queued in a pending buffer until the transport takes
//! them (partial sends are normal on non-blocking stacks).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::info;
#[cfg(debug_assertions)]
use tracing::debug;

use super::app::Application;
use super::framer::{Framer, FramerResult};
use super::parser::Parser;
use super::recv_buffer::RecvBuffer;
use super::session::{Outbox, Session, SessionConfig, SessionState, Status};
use super::store::Journal;
use super::transport::{Clock, Transport};

/// Receive buffer size: 16 KB, the largest frame the framer will accept.
pub const RECV_SIZE: usize = super::flat_index::RECV_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The session ended it (Logout, timeout, protocol error).
    Session,
    /// The peer closed the stream.
    PeerClosed,
    /// `stop` was set and the logout finished (or timed out).
    Stopped,
}

/// Outbound bytes not yet taken by the transport.
struct Pending {
    buf: Vec<u8>,
    /// Bytes at the front already sent.
    sent: usize,
}

impl Outbox for Pending {
    fn send(&mut self, message: &[u8]) {
        #[cfg(debug_assertions)]
        debug!(out = %String::from_utf8_lossy(message).replace('\x01', "|"));
        self.buf.extend_from_slice(message);
    }
}

pub struct Connection<T: Transport, C: Clock, A: Application> {
    transport: T,
    clock: C,
    app: A,
    recv: RecvBuffer<RECV_SIZE>,
    framer: Framer,
    parser: Box<Parser>,
    session: Session,
    pending: Pending,
}

impl<T: Transport, C: Clock, A: Application> Connection<T, C, A> {
    pub fn new(transport: T, clock: C, config: SessionConfig, app: A) -> io::Result<Self> {
        let session = Session::new(config, clock.now_ns());
        Self::with_session(transport, clock, session, app)
    }

    /// A connection whose session keeps its sequence numbers and sent
    /// messages in `journal`.
    pub fn with_journal(transport: T, clock: C, config: SessionConfig, journal: Journal, app: A) -> io::Result<Self> {
        let session = Session::with_journal(config, journal, clock.now_ns());
        Self::with_session(transport, clock, session, app)
    }

    fn with_session(transport: T, clock: C, session: Session, app: A) -> io::Result<Self> {
        let version = session.config().version;
        Ok(Self {
            transport,
            clock,
            app,
            recv: RecvBuffer::new()?,
            framer: Framer::new(version),
            parser: Box::new(Parser::new()),
            session,
            // Allocated once; only grows if the peer stops reading.
            pending: Pending { buf: Vec::with_capacity(64 * 1024), sent: 0 },
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn app(&self) -> &A {
        &self.app
    }

    /// Runs until the session ends or the peer goes away. Setting `stop` starts
    /// an orderly Logout.
    pub fn run(&mut self, stop: &AtomicBool) -> io::Result<CloseReason> {
        let mut logout_requested = false;

        if self.session.start(self.clock.now_ns(), &mut self.pending) == Status::Disconnect {
            return self.finish(CloseReason::Session)
        }
        let mut last_state = self.session.state();
        info!(state = ?last_state, "session started");

        loop {
            let now = self.clock.now_ns();

            if stop.load(Ordering::Relaxed) && !logout_requested {
                logout_requested = true;
                if self.session.logout("Shutting down", now, &mut self.pending) == Status::Disconnect {
                    return self.finish(CloseReason::Stopped)
                }
            }
            if self.session.on_timer(now, &mut self.pending) == Status::Disconnect {
                return self.finish(if logout_requested { CloseReason::Stopped } else { CloseReason::Session })
            }
            if self.session.state() == SessionState::Active {
                self.app.on_timer(&mut self.session.sender(now, &mut self.pending));
            }
            if let Some(journal) = self.session.journal_mut() {
                // Timer flushes, and a failed per-message flush: the journal
                // can't be trusted any more, so stop.
                journal.on_timer(now)?;
            }
            self.flush()?;

            if self.recv.spare_len() == 0 {
                // The framer drops garbage and oversized frames, so a full
                // buffer means something is badly wrong.
                return Err(io::Error::other("receive buffer full"))
            }
            match self.transport.recv(self.recv.spare_mut()) {
                Ok(0) => {
                    info!(state = ?self.session.state(), "peer closed the connection");
                    return Ok(CloseReason::PeerClosed)
                }
                Ok(n) => self.recv.commit(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }

            let now = self.clock.now_ns();
            loop {
                let FramerResult::Frame { buffer } = self.framer.frame(&mut self.recv) else { break };
                let len = buffer.len();
                #[cfg(debug_assertions)]
                debug!(r#in = %String::from_utf8_lossy(buffer).replace('\x01', "|"));
                let status = self.session.on_frame(self.parser.parse_frame(buffer), now, &mut self.pending, &mut self.app);
                self.recv.consume(len);

                let state = self.session.state();
                if state != last_state {
                    info!(from = ?last_state, to = ?state, in_seq = self.session.next_in_seq(), out_seq = self.session.next_out_seq(), "session state");
                    last_state = state;
                }
                if status == Status::Disconnect {
                    return self.finish(if logout_requested { CloseReason::Stopped } else { CloseReason::Session })
                }
            }
            self.flush()?;
        }
    }

    /// Flushes what's pending (best effort) before closing.
    fn finish(&mut self, reason: CloseReason) -> io::Result<CloseReason> {
        info!(?reason, state = ?self.session.state(), "closing connection");
        let _ = self.flush_all();
        Ok(reason)
    }

    /// Sends as much pending output as the transport takes right now.
    fn flush(&mut self) -> io::Result<()> {
        let p = &mut self.pending;
        while p.sent < p.buf.len() {
            match self.transport.send(&p.buf[p.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => p.sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e),
            }
        }
        if p.sent == p.buf.len() {
            p.buf.clear();
            p.sent = 0;
        }
        Ok(())
    }

    /// Keeps flushing until everything is out (used when closing).
    fn flush_all(&mut self) -> io::Result<()> {
        for _ in 0..1000 {
            self.flush()?;
            if self.pending.buf.is_empty() {
                return Ok(())
            }
        }
        Err(io::ErrorKind::TimedOut.into())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::FixVersion;
    use crate::fix::app::NoApplication;
    use crate::fix::decode::Decimal;
    use crate::fix::demo::{OneOrder, OrderAcker, OrderSpec};
    use crate::fix::spec::{ExecType, OrdStatus, Side};
    use crate::fix::encoder::MessageWriter;
    use crate::fix::parser::ParseOutcome;
    use crate::fix::session::Role;
    use crate::fix::spec::tags;
    use crate::fix::transport::{StdTcp, SystemClock};
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// Scripted counterparty over a real socket: writes frames with chosen
    /// sequence numbers and reads the gateway's replies.
    struct Client {
        stream: TcpStream,
        recv: RecvBuffer<RECV_SIZE>,
        framer: Framer,
        parser: Box<Parser>,
        inbox: VecDeque<String>,
    }

    impl Client {
        fn connect(addr: std::net::SocketAddr) -> Self {
            let stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
            Self {
                stream,
                recv: RecvBuffer::new().unwrap(),
                framer: Framer::new(FixVersion::Fix44),
                parser: Box::new(Parser::new()),
                inbox: VecDeque::new(),
            }
        }

        fn send(&mut self, msg_type: &str, seq: u32, body: &[(u16, &str)]) {
            let mut buf = [0u8; 1024];
            let mut w = MessageWriter::new(&mut buf, FixVersion::Fix44, msg_type.as_bytes()).unwrap();
            w.header(b"CLIENT", b"GATEWAY", seq, SystemClock.now_ns()).unwrap();
            for (tag, value) in body {
                w.str(*tag, value).unwrap();
            }
            self.stream.write_all(w.finish().unwrap()).unwrap();
        }

        /// Next message from the gateway (as '|' text), waiting up to `secs`.
        fn next(&mut self, secs: u64) -> Option<String> {
            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            loop {
                if let Some(m) = self.inbox.pop_front() {
                    return Some(m)
                }
                if std::time::Instant::now() > deadline {
                    return None
                }
                match self.stream.read(self.recv.spare_mut()) {
                    Ok(0) => return None,
                    Ok(n) => self.recv.commit(n),
                    Err(_) => continue,
                }
                while let FramerResult::Frame { buffer } = self.framer.frame(&mut self.recv) {
                    let len = buffer.len();
                    assert!(matches!(self.parser.parse_frame(buffer), ParseOutcome::Message(_)), "gateway sent a bad frame");
                    self.inbox.push_back(String::from_utf8_lossy(buffer).replace('\x01', "|"));
                    self.recv.consume(len);
                }
            }
        }

        /// Next message of `msg_type`, skipping heartbeats and the like.
        fn expect(&mut self, msg_type: &str, secs: u64) -> String {
            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                if let Some(m) = self.next(1) {
                    if field(&m, 35) == Some(msg_type) {
                        return m
                    }
                }
            }
            panic!("no 35={msg_type} within {secs}s")
        }

        fn closed(&mut self, secs: u64) -> bool {
            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            let mut scratch = [0u8; 512];
            while std::time::Instant::now() < deadline {
                match self.stream.read(&mut scratch) {
                    Ok(0) => return true,
                    _ => continue,
                }
            }
            false
        }
    }

    fn field(msg: &str, tag: u16) -> Option<&str> {
        let prefix = format!("|{tag}=");
        let start = msg.find(&prefix)? + prefix.len();
        Some(&msg[start..start + msg[start..].find('|')?])
    }

    fn spawn_acceptor() -> (std::net::SocketAddr, thread::JoinHandle<io::Result<CloseReason>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept()?;
            let transport = StdTcp::new(stream, Duration::from_millis(50))?;
            let config = SessionConfig::new(Role::Acceptor, "GATEWAY", "CLIENT");
            let mut conn = Connection::new(transport, SystemClock, config, OrderAcker::default())?;
            conn.run(&AtomicBool::new(false))
        });
        (addr, handle)
    }

    #[test]
    fn acceptor_end_to_end_over_tcp() {
        let (addr, server) = spawn_acceptor();
        let mut c = Client::connect(addr);

        // Logon with a 1 s heartbeat so the timers fire quickly.
        c.send("A", 1, &[(tags::ENCRYPT_METHOD, "0"), (tags::HEART_BT_INT, "1")]);
        let logon = c.expect("A", 2);
        assert_eq!((field(&logon, 34), field(&logon, 108)), (Some("1"), Some("1")));

        // TestRequest -> Heartbeat echoing the id.
        c.send("1", 2, &[(tags::TEST_REQ_ID, "ping-1")]);
        let hb = c.expect("0", 2);
        assert_eq!(field(&hb, 112), Some("ping-1"));

        // Our silence: the gateway sends a TestRequest; answer it.
        let tr = c.expect("1", 4);
        let id = field(&tr, 112).unwrap().to_string();
        c.send("0", 3, &[(tags::TEST_REQ_ID, &id)]);

        // Gap: 6 arrives while 4 is expected -> ResendRequest(4, 0).
        c.send("0", 6, &[]);
        let rr = c.expect("2", 2);
        assert_eq!((field(&rr, 7), field(&rr, 16)), (Some("4"), Some("0")));
        // Fill it and carry on.
        c.send("4", 4, &[(tags::POSS_DUP_FLAG, "Y"), (tags::ORIG_SENDING_TIME, "20261025-00:00:00.000"), (tags::GAP_FILL_FLAG, "Y"), (tags::NEW_SEQ_NO, "7")]);
        c.send("1", 7, &[(tags::TEST_REQ_ID, "ping-2")]);
        assert_eq!(field(&c.expect("0", 2), 112), Some("ping-2"));

        // An order -> ExecutionReport; a limit order without a price -> BusinessMessageReject.
        c.send("D", 8, &[(tags::CL_ORD_ID, "ord-1"), (tags::SYMBOL, "MSFT"), (tags::SIDE, "1"), (tags::TRANSACT_TIME, "20261025-17:00:00"),
                         (tags::ORDER_QTY, "100"), (tags::ORD_TYPE, "2"), (tags::PRICE, "412.50")]);
        let er = c.expect("8", 2);
        assert_eq!((field(&er, 11), field(&er, 150), field(&er, 39), field(&er, 44)), (Some("ord-1"), Some("0"), Some("0"), Some("412.50")));
        c.send("D", 9, &[(tags::CL_ORD_ID, "ord-2"), (tags::SYMBOL, "MSFT"), (tags::SIDE, "1"), (tags::TRANSACT_TIME, "20261025-17:00:00"),
                         (tags::ORDER_QTY, "100"), (tags::ORD_TYPE, "2")]);
        let j = c.expect("j", 2);
        assert_eq!((field(&j, 45), field(&j, 380), field(&j, 379)), (Some("9"), Some("5"), Some("ord-2")));

        // Our ResendRequest -> gap fill up to the gateway's next sequence number.
        c.send("2", 10, &[(tags::BEGIN_SEQ_NO, "1"), (tags::END_SEQ_NO, "0")]);
        let fill = c.expect("4", 2);
        assert_eq!((field(&fill, 34), field(&fill, 43), field(&fill, 123)), (Some("1"), Some("Y"), Some("Y")));
        assert!(field(&fill, 36).unwrap().parse::<u32>().unwrap() > 1);

        // Logout -> Logout reply, then the gateway closes the socket.
        c.send("5", 11, &[(tags::TEXT, "bye")]);
        c.expect("5", 2);
        assert!(c.closed(2));
        assert_eq!(server.join().unwrap().unwrap(), CloseReason::Session);
    }

    #[test]
    fn initiator_and_acceptor_talk_to_each_other() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let config = SessionConfig::new(Role::Acceptor, "GATEWAY", "CLIENT");
            let mut conn = Connection::new(StdTcp::new(stream, Duration::from_millis(50)).unwrap(), SystemClock, config, OrderAcker::default()).unwrap();
            let reason = conn.run(&AtomicBool::new(false)).unwrap();
            (reason, conn.session().next_in_seq(), conn.app().reports)
        });

        let stop = Arc::new(AtomicBool::new(false));
        let client_stop = stop.clone();
        let client = thread::spawn(move || {
            let stream = TcpStream::connect(addr).unwrap();
            let mut config = SessionConfig::new(Role::Initiator, "CLIENT", "GATEWAY");
            config.heart_bt_int_secs = 1;
            config.reset_on_logon = true;
            let order = OrderSpec { cl_ord_id: "c1".into(), symbol: "MSFT".into(), side: Side::Buy, qty: Decimal::new(100, 0), price: None };
            let mut conn = Connection::new(StdTcp::new(stream, Duration::from_millis(50)).unwrap(), SystemClock, config, OneOrder::new(order)).unwrap();
            let reason = conn.run(&client_stop).unwrap();
            (reason, conn.session().next_out_seq(), conn.app().reports.clone())
        });

        // Let them exchange a few heartbeats, then shut the initiator down.
        thread::sleep(Duration::from_millis(2500));
        stop.store(true, Ordering::Relaxed);

        let (client_reason, client_out, client_reports) = client.join().unwrap();
        let (server_reason, server_in, server_reports) = server.join().unwrap();
        assert_eq!(client_reason, CloseReason::Stopped);
        assert_eq!(server_reason, CloseReason::Session);
        // Logon + the order + at least two heartbeats + Logout, all accepted in order.
        assert!(client_out >= 6, "client sent {}", client_out - 1);
        assert_eq!(server_in, client_out);
        // The order was acknowledged.
        assert_eq!(server_reports, 1);
        assert_eq!(client_reports, [(ExecType::New, OrdStatus::New)]);
    }

    /// Transport that takes at most 7 bytes per send, to exercise partial writes.
    struct Trickle {
        inner: StdTcp,
    }

    impl Transport for Trickle {
        fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.recv(buf)
        }
        fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.send(&buf[..buf.len().min(7)])
        }
    }

    #[test]
    fn partial_sends() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let transport = Trickle { inner: StdTcp::new(stream, Duration::from_millis(50)).unwrap() };
            let mut conn = Connection::new(transport, SystemClock, SessionConfig::new(Role::Acceptor, "GATEWAY", "CLIENT"), NoApplication).unwrap();
            conn.run(&AtomicBool::new(false)).unwrap()
        });
        let mut c = Client::connect(addr);
        c.send("A", 1, &[(tags::ENCRYPT_METHOD, "0"), (tags::HEART_BT_INT, "30")]);
        c.expect("A", 2);
        for seq in 2..12 {
            c.send("1", seq, &[(tags::TEST_REQ_ID, &format!("t{seq}"))]);
        }
        for seq in 2..12 {
            assert_eq!(field(&c.expect("0", 2), 112), Some(format!("t{seq}").as_str()));
        }
        c.send("5", 12, &[]);
        c.expect("5", 2);
        assert_eq!(server.join().unwrap(), CloseReason::Session);
    }

    /// An acceptor with a journal in `dir`, taking `connections` connections
    /// one after the other; returns each one's close reason.
    fn spawn_journaled_acceptor(dir: std::path::PathBuf, flush: crate::fix::store::FlushPolicy, connections: usize)
        -> (std::net::SocketAddr, thread::JoinHandle<Vec<CloseReason>>)
    {
        use crate::fix::store::JournalConfig;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut reasons = Vec::new();
            for _ in 0..connections {
                let (stream, _) = listener.accept().unwrap();
                let journal_config = JournalConfig { dir: dir.clone(), entries: 1024, capacity: 1 << 20, flush, lock: false };
                let journal = Journal::open(&journal_config, "GATEWAY", "CLIENT", SystemClock.now_ns()).unwrap();
                let config = SessionConfig::new(Role::Acceptor, "GATEWAY", "CLIENT");
                let transport = StdTcp::new(stream, Duration::from_millis(50)).unwrap();
                let mut conn = Connection::with_journal(transport, SystemClock, config, journal, OrderAcker::default()).unwrap();
                reasons.push(conn.run(&AtomicBool::new(false)).unwrap());
            }
            reasons
        });
        (addr, handle)
    }

    fn order(c: &mut Client, seq: u32, id: &str) {
        c.send("D", seq, &[(tags::CL_ORD_ID, id), (tags::SYMBOL, "MSFT"), (tags::SIDE, "1"), (tags::TRANSACT_TIME, "20261025-17:00:00"),
                           (tags::ORDER_QTY, "100"), (tags::ORD_TYPE, "1")]);
    }

    #[test]
    fn resends_stored_messages_and_resumes_after_reconnect() {
        use crate::fix::store::{FlushPolicy, TestDir};
        for flush in [FlushPolicy::Never, FlushPolicy::Timer(Duration::from_millis(20)), FlushPolicy::EveryMessage] {
            let dir = TestDir::new("loopback-resend");
            let (addr, server) = spawn_journaled_acceptor(dir.0.clone(), flush, 3);

            let mut c = Client::connect(addr);
            c.send("A", 1, &[(tags::ENCRYPT_METHOD, "0"), (tags::HEART_BT_INT, "30")]);
            assert_eq!(field(&c.expect("A", 2), 34), Some("1"));
            order(&mut c, 2, "ord-1");
            let er1 = c.expect("8", 2);
            c.send("1", 3, &[(tags::TEST_REQ_ID, "t")]);
            c.expect("0", 2);
            order(&mut c, 4, "ord-2");
            let er2 = c.expect("8", 2);
            assert_eq!((field(&er1, 34), field(&er2, 34)), (Some("2"), Some("4")));

            // Everything again: Logon and Heartbeat gap filled, the reports resent.
            c.send("2", 5, &[(tags::BEGIN_SEQ_NO, "1"), (tags::END_SEQ_NO, "0")]);
            let replay: Vec<String> = (0..4).map(|_| c.next(2).expect("resend")).collect();
            let summary: Vec<_> = replay.iter().map(|m| (field(m, 35).unwrap(), field(m, 34).unwrap())).collect();
            assert_eq!(summary, [("4", "1"), ("8", "2"), ("4", "3"), ("8", "4")]);
            for (orig, again) in [(&er1, &replay[1]), (&er2, &replay[3])] {
                assert_eq!((field(again, 43), field(again, 122)), (Some("Y"), field(orig, 52)));
                assert_eq!((field(again, 11), field(again, 37), field(again, 17)), (field(orig, 11), field(orig, 37), field(orig, 17)));
            }
            c.send("5", 6, &[]);
            c.expect("5", 2);
            assert!(c.closed(2));

            // Reconnect: both sides carry on, and the old reports are still there.
            let mut c = Client::connect(addr);
            c.send("A", 1, &[(tags::ENCRYPT_METHOD, "0"), (tags::HEART_BT_INT, "30")]);
            assert_eq!(field(&c.expect("5", 2), 58), Some("MsgSeqNum too low"), "the gateway remembers our sequence numbers");
            assert!(c.closed(2));
            drop(c);
            let mut c = Client::connect(addr);
            c.send("A", 7, &[(tags::ENCRYPT_METHOD, "0"), (tags::HEART_BT_INT, "30")]);
            let logon = c.expect("A", 2);
            assert_eq!(field(&logon, 34), Some("7"), "{flush:?}: Logon 1, ER 2, Heartbeat 3, ER 4, Logout 5, Logout 6");
            c.send("2", 8, &[(tags::BEGIN_SEQ_NO, "2"), (tags::END_SEQ_NO, "2")]);
            let again = c.expect("8", 2);
            assert_eq!((field(&again, 34), field(&again, 11), field(&again, 43)), (Some("2"), Some("ord-1"), Some("Y")));
            c.send("5", 9, &[]);
            c.expect("5", 2);
            assert_eq!(server.join().unwrap(), [CloseReason::Session; 3]);
        }
    }
}
