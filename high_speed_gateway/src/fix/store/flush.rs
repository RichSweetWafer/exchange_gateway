//! When and how the journal reaches the disk.
//!
//! The journal only ever writes to its mapping; the `FlushPolicy` decides when
//! the dirty parts are handed to a `Flusher`, which makes them durable. `Msync`
//! is the default; an io_uring or O_DIRECT flusher can take its place, since
//! each region carries both the mapped bytes and their place in the file.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushPolicy {
    /// Leave it to the kernel's writeback. A process crash loses nothing (the
    /// pages are in the page cache); a kernel crash or power cut can lose the
    /// latest messages and sequence numbers.
    Never,
    /// From the connection's timer path, at most once per interval and only if
    /// something changed. Never on the send path.
    Timer(Duration),
    /// After every message sent or received, before the session call returns.
    /// One synchronous flush per message: durable, and as slow as the disk.
    EveryMessage,
}

/// A page-aligned part of the journal file to make durable.
#[derive(Debug, Clone, Copy)]
pub struct Region<'a> {
    /// Where `bytes` live in the file.
    pub file_offset: u64,
    /// The mapped memory holding them.
    pub bytes: &'a [u8],
}

/// Makes regions of the journal durable.
pub trait Flusher: Send {
    fn flush(&mut self, fd: RawFd, regions: &[Region<'_>]) -> io::Result<()>;
}

/// msync(MS_SYNC) on each region: returns once the pages are on the disk.
#[derive(Debug, Default, Clone, Copy)]
pub struct Msync;

impl Flusher for Msync {
    fn flush(&mut self, _fd: RawFd, regions: &[Region<'_>]) -> io::Result<()> {
        for r in regions {
            // SAFETY: the region is a page-aligned part of a live mapping.
            if unsafe { libc::msync(r.bytes.as_ptr() as *mut libc::c_void, r.bytes.len(), libc::MS_SYNC) } != 0 {
                return Err(io::Error::last_os_error())
            }
        }
        Ok(())
    }
}
