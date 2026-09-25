//! I/O and time behind traits, so the connection loop can run on std TCP now
//! and on a busy-polled or kernel-bypass stack (ef_vi, TCPDirect, DPDK) later.
//! Nothing above this layer touches std::net or the system clock.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A byte stream to one counterparty.
///
/// Both calls may return `ErrorKind::WouldBlock` when nothing can be done right
/// now. A blocking implementation waits a short, bounded time first (so the
/// loop still gets to run its timers); a busy-poll implementation returns at
/// once.
pub trait Transport {
    /// Reads into `buf`. `Ok(0)` means the peer closed the stream.
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Writes a prefix of `buf` and returns its length; partial writes are fine.
    fn send(&mut self, buf: &[u8]) -> io::Result<usize>;
}

/// Wall-clock time in nanoseconds since the Unix epoch (FIX SendingTime is UTC).
pub trait Clock {
    fn now_ns(&self) -> i64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[inline(always)]
    fn now_ns(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
    }
}

/// std TcpStream, blocking with a read timeout. The timeout is how often the
/// loop wakes up for timers when the line is quiet.
pub struct StdTcp {
    stream: TcpStream,
}

impl StdTcp {
    pub fn new(stream: TcpStream, poll_interval: Duration) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(poll_interval))?;
        Ok(Self { stream })
    }

    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }
}

impl Transport for StdTcp {
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.read(buf) {
            // A read timeout shows up as WouldBlock or TimedOut depending on the platform.
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(io::ErrorKind::WouldBlock.into()),
            other => other,
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
}
