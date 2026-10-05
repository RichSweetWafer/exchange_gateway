//! Per-session message journal: our outbound application messages and both
//! sequence numbers in one memory-mapped file, so a restarted session carries
//! on where it stopped and answers ResendRequests with the real messages.
//!
//! File layout (every part page aligned):
//!
//! ```text
//! | header page | index: N x 32-byte Entry | data ring: C bytes |
//! ```
//!
//! The header holds the magic, the geometry, both CompIDs, both sequence
//! numbers, the creation time and the data ring's write cursor. Index slot
//! `seq % N` describes outbound message `seq`: its offset in the data ring,
//! length, SendingTime and where its body starts. Only application messages
//! are stored; an admin message's slot keeps an older entry whose seq doesn't
//! match, so it reads back as missing (and gets gap filled), as does anything
//! older than the last N messages or overwritten in the data ring.
//!
//! The data ring is mapped twice back to back, like RecvBuffer, so a message
//! that wraps the physical end is still one contiguous slice.
//!
//! Appending is a memcpy plus a 32-byte index write, and sequence numbers are
//! plain stores into the header: no syscalls, no allocation. Durability is up
//! to the `FlushPolicy` and its `Flusher` (see flush.rs).

mod flush;
mod journal;

pub use flush::{FlushPolicy, Flusher, Msync, Region};

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::atomic::{Ordering, compiler_fence};

use super::encoder::TRAILER_LEN;

const MAGIC: [u8; 8] = *b"FIXJRNL\0";
const VERSION: u32 = 1;
/// Longest CompID the header can hold.
pub const MAX_COMP_ID: usize = 64;

#[repr(C)]
struct Header {
    magic: [u8; 8],
    version: u32,
    entry_size: u32,
    entries: u64,
    capacity: u64,
    /// When the journal was created or last reset, ns since the Unix epoch.
    created_ns: i64,
    next_in_seq: u32,
    next_out_seq: u32,
    /// Data ring write cursor, monotonic; the physical offset is `& (C - 1)`.
    data_head: u64,
    our_len: u8,
    their_len: u8,
    _pad: [u8; 6],
    our_comp_id: [u8; MAX_COMP_ID],
    their_comp_id: [u8; MAX_COMP_ID],
}

/// One stored message. seq 0 (a zeroed slot) is empty.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Data ring cursor of its first byte.
    offset: u64,
    sending_time: i64,
    seq: u32,
    len: u32,
    /// Offset of the first field after the session's header fields (49, 56, 34, 52).
    body_start: u32,
    _pad: u32,
}

const ENTRY_SIZE: usize = size_of::<Entry>();
const _: () = assert!(ENTRY_SIZE == 32 && size_of::<Header>() <= 4096);



/// A message read back from the journal, borrowed from the mapping.
#[derive(Debug, Clone, Copy)]
pub struct StoredMessage<'a> {
    pub seq: u32,
    /// Its original SendingTime, ns since the Unix epoch.
    pub sending_time: i64,
    /// The whole message as it was sent.
    pub bytes: &'a [u8],
    body_start: usize,
}

impl<'a> StoredMessage<'a> {
    /// MsgType (35), the third field.
    pub fn msg_type(&self) -> Option<&'a [u8]> {
        let mut rest = self.bytes;
        for _ in 0..2 {
            rest = &rest[memchr::memchr(0x01, rest)? + 1..];
        }
        let value = rest.strip_prefix(b"35=")?;
        Some(&value[..memchr::memchr(0x01, value)?])
    }

    /// Everything after the session's header fields, without the trailer.
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[self.body_start..self.bytes.len() - TRAILER_LEN]
    }
}

pub struct Journal {
    /// Holds the flock for as long as the journal is open.
    file: File,
    path: PathBuf,
    base: NonNull<u8>,
    map_len: usize,
    data_off: usize,
    entries: u32,
    capacity: usize,
    page: usize,

    policy: FlushPolicy,
    flusher: Box<dyn Flusher>,
    /// Data ring cursor up to which everything has been flushed.
    flushed_head: u64,
    /// Seqs appended since the last flush (first, last).
    dirty_seqs: Option<(u32, u32)>,
    header_dirty: bool,
    /// After a reset: flush the whole index.
    index_dirty: bool,
    last_flush: i64,
    /// A failed EveryMessage flush, reported by the next `on_timer`.
    error: Option<io::Error>,
}

// The mapping is owned by the journal and only touched through `&mut self`.
unsafe impl Send for Journal {}

impl Journal {
    /// Opens the session's journal in `config.dir`, creating it (sequence
    /// numbers 1 / 1) if it doesn't exist. An existing file must have the same
    /// geometry and CompIDs. Fails if another process has it open.
    pub fn open(config: &JournalConfig, our_comp_id: &str, their_comp_id: &str, now: i64) -> io::Result<Self> {
        Self::open_with(config, our_comp_id, their_comp_id, now, Box::new(Msync))
    }

    pub fn open_with(config: &JournalConfig, our_comp_id: &str, their_comp_id: &str, now: i64,
                     flusher: Box<dyn Flusher>) -> io::Result<Self> {
        let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidInput, msg);
        let page = page_size();
        if config.entries == 0 {
            return Err(invalid("journal needs at least one index entry".into()))
        }
        if !config.capacity.is_power_of_two() || !config.capacity.is_multiple_of(page) {
            return Err(invalid(format!("journal capacity {} must be a power of two and a multiple of {page}", config.capacity)))
        }
        for id in [our_comp_id, their_comp_id] {
            if id.is_empty() || id.len() > MAX_COMP_ID {
                return Err(invalid(format!("CompID {id:?} must be 1 to {MAX_COMP_ID} bytes")))
            }
        }

        let index_len = (config.entries as usize * ENTRY_SIZE).next_multiple_of(page);
        let data_off = page + index_len;
        let file_len = data_off + config.capacity;

        fs::create_dir_all(&config.dir)?;
        let path = config.dir.join(file_name(our_comp_id, their_comp_id));
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        let fd = file.as_raw_fd();
        // SAFETY: plain syscalls on our own fd.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(e.kind(), format!("{}: journal in use ({e})", path.display())))
        }
        let existing = file.metadata()?.len();
        let fresh = existing == 0;
        if fresh {
            allocate(fd, file_len)?;
        } else if existing != file_len as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!(
                "{}: {existing} bytes, expected {file_len} for {} entries and {} data bytes; reset or remove it to change the geometry",
                path.display(), config.entries, config.capacity)))
        }

        // SAFETY: the file is file_len bytes long and stays open with the mapping.
        let base = unsafe { map(fd, data_off, config.capacity)? };
        let mut journal = Self {
            file,
            path,
            base,
            map_len: data_off + 2 * config.capacity,
            data_off,
            entries: config.entries,
            capacity: config.capacity,
            page,
            policy: config.flush,
            flusher,
            flushed_head: 0,
            dirty_seqs: None,
            header_dirty: false,
            index_dirty: false,
            last_flush: now,
            error: None,
        };

        if fresh {
            let h = journal.header_mut();
            h.magic = MAGIC;
            h.version = VERSION;
            h.entry_size = ENTRY_SIZE as u32;
            h.entries = config.entries as u64;
            h.capacity = config.capacity as u64;
            h.our_len = our_comp_id.len() as u8;
            h.their_len = their_comp_id.len() as u8;
            h.our_comp_id[..our_comp_id.len()].copy_from_slice(our_comp_id.as_bytes());
            h.their_comp_id[..their_comp_id.len()].copy_from_slice(their_comp_id.as_bytes());
            journal.reset(now);
            // The header has to be on disk before anything refers to it.
            journal.flush()?;
        } else {
            journal.check_header(config, our_comp_id, their_comp_id)?;
            journal.flushed_head = journal.header().data_head;
        }
        if config.lock {
            journal.lock()?;
        }
        Ok(journal)
    }

    fn check_header(&self, config: &JournalConfig, our: &str, their: &str) -> io::Result<()> {
        let h = self.header();
        let problem = if h.magic != MAGIC {
            "not a FIX journal"
        } else if h.version != VERSION || h.entry_size != ENTRY_SIZE as u32 {
            "unsupported journal version"
        } else if h.entries != config.entries as u64 || h.capacity != config.capacity as u64 {
            "different geometry; reset or remove it to change it"
        } else if &h.our_comp_id[..h.our_len as usize] != our.as_bytes() || &h.their_comp_id[..h.their_len as usize] != their.as_bytes() {
            "belongs to other CompIDs"
        } else {
            return Ok(())
        };
        Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {problem}", self.path.display())))
    }

    /// Locks the mapping in RAM so appends never page-fault.
    pub fn lock(&self) -> io::Result<()> {
        // SAFETY: our own mapping.
        if unsafe { libc::mlock(self.base.as_ptr().cast(), self.map_len) } != 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> u32 {
        self.entries
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn next_in_seq(&self) -> u32 {
        self.header().next_in_seq
    }

    pub fn next_out_seq(&self) -> u32 {
        self.header().next_out_seq
    }

    /// When the journal was created or last reset.
    pub fn created_ns(&self) -> i64 {
        self.header().created_ns
    }

    /// Records both sequence numbers. With `EveryMessage`, flushes everything
    /// dirty before returning; a flush error comes back from `on_timer`.
    #[inline]
    pub fn set_seqs(&mut self, next_in: u32, next_out: u32) {
        let h = self.header_mut();
        if h.next_in_seq != next_in || h.next_out_seq != next_out {
            h.next_in_seq = next_in;
            h.next_out_seq = next_out;
            self.header_dirty = true;
        }
        if self.policy == FlushPolicy::EveryMessage && self.is_dirty()
            && let Err(e) = self.flush()
        {
            self.error.get_or_insert(e);
        }
    }

    /// Stores outbound message `seq`, replacing whatever held its index slot.
    /// `body_start` is the offset of its first field after the session's
    /// header fields. A message longer than the data ring isn't stored.
    #[inline]
    pub fn append(&mut self, seq: u32, sending_time: i64, msg: &[u8], body_start: usize) {
        if seq == 0 || msg.len() > self.capacity || body_start + TRAILER_LEN > msg.len() {
            return
        }
        let head = self.header().data_head;
        let at = head as usize & (self.capacity - 1);
        // SAFETY: at < C and len <= C, inside the mirrored 2C data mapping.
        unsafe { ptr::copy_nonoverlapping(msg.as_ptr(), self.data_ptr().add(at), msg.len()) };
        let entry = Entry { offset: head, sending_time, seq, len: msg.len() as u32, body_start: body_start as u32, _pad: 0 };
        // SAFETY: slot < N, inside the index.
        unsafe { self.entry_ptr(seq).write(entry) };
        // Data and entry before the cursor that makes them reachable.
        compiler_fence(Ordering::Release);
        self.header_mut().data_head = head + msg.len() as u64;
        self.dirty_seqs = Some(match self.dirty_seqs {
            Some((first, _)) => (first, seq),
            None => (seq, seq),
        });
    }

    /// Outbound message `seq`, if it's still stored.
    pub fn get(&self, seq: u32) -> Option<StoredMessage<'_>> {
        // SAFETY: slot < N, inside the index.
        let e = unsafe { self.entry_ptr(seq).read() };
        let head = self.header().data_head;
        let len = e.len as usize;
        let intact = seq != 0 && e.seq == seq && len <= self.capacity && (e.body_start as usize) + TRAILER_LEN <= len
            && e.offset.checked_add(len as u64).is_some_and(|end| end <= head)
            && head - e.offset <= self.capacity as u64;
        if !intact {
            return None
        }
        let at = e.offset as usize & (self.capacity - 1);
        // SAFETY: at < C and len <= C, inside the mirrored data mapping.
        let bytes = unsafe { std::slice::from_raw_parts(self.data_ptr().add(at), len) };
        Some(StoredMessage { seq, sending_time: e.sending_time, bytes, body_start: e.body_start as usize })
    }

    /// Starts over: sequence numbers 1 / 1, nothing stored. For a sequence
    /// reset (ResetSeqNumFlag=Y).
    pub fn reset(&mut self, now: i64) {
        // SAFETY: the index is entries * ENTRY_SIZE bytes from the second page.
        unsafe { ptr::write_bytes(self.base.as_ptr().add(self.page), 0, self.entries as usize * ENTRY_SIZE) };
        let h = self.header_mut();
        h.next_in_seq = 1;
        h.next_out_seq = 1;
        h.data_head = 0;
        h.created_ns = now;
        self.flushed_head = 0;
        self.dirty_seqs = None;
        self.header_dirty = true;
        self.index_dirty = true;
    }

    /// For the connection's timer path: flushes on the `Timer` policy's
    /// schedule and reports a failed `EveryMessage` flush.
    pub fn on_timer(&mut self, now: i64) -> io::Result<()> {
        if let Some(e) = self.error.take() {
            return Err(e)
        }
        if let FlushPolicy::Timer(interval) = self.policy
            && now - self.last_flush >= interval.as_nanos() as i64
        {
            self.last_flush = now;
            if self.is_dirty() {
                return self.flush()
            }
        }
        Ok(())
    }

    fn is_dirty(&self) -> bool {
        self.header_dirty || self.index_dirty || self.dirty_seqs.is_some() || self.flushed_head != self.header().data_head
    }

    /// Hands everything written since the last flush to the flusher.
    pub fn flush(&mut self) -> io::Result<()> {
        let mut regions = [Region { file_offset: 0, bytes: &[] }; 5];
        let mut n = 0;
        let mut add = |start: usize, end: usize| {
            // Page-aligned, so any flusher (msync, O_DIRECT) can take it as is.
            let start = start / self.page * self.page;
            let end = end.next_multiple_of(self.page);
            // SAFETY: start..end is inside the first copy of the file mapping.
            let bytes = unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(start), end - start) };
            regions[n] = Region { file_offset: start as u64, bytes };
            n += 1;
        };

        let head = self.header().data_head;
        // The header has the data cursor, so it goes with any new message.
        if self.header_dirty || self.dirty_seqs.is_some() || head != self.flushed_head {
            add(0, self.page);
        }
        let n_entries = self.entries as usize;
        let slot_bytes = |slot: usize| self.page + slot * ENTRY_SIZE;
        if self.index_dirty {
            add(self.page, slot_bytes(n_entries));
        } else if let Some((first, last)) = self.dirty_seqs {
            if (last - first) as usize >= n_entries - 1 {
                add(self.page, slot_bytes(n_entries));
            } else {
                let (a, b) = (first as usize % n_entries, last as usize % n_entries);
                if a <= b {
                    add(slot_bytes(a), slot_bytes(b + 1));
                } else {
                    add(slot_bytes(a), slot_bytes(n_entries));
                    add(self.page, slot_bytes(b + 1));
                }
            }
        }
        if head > self.flushed_head {
            let cap = self.capacity;
            if head - self.flushed_head >= cap as u64 {
                add(self.data_off, self.data_off + cap);
            } else {
                let (a, b) = (self.flushed_head as usize & (cap - 1), head as usize & (cap - 1));
                if a < b {
                    add(self.data_off + a, self.data_off + b);
                } else {
                    add(self.data_off + a, self.data_off + cap);
                    if b > 0 {
                        add(self.data_off, self.data_off + b);
                    }
                }
            }
        }

        if n > 0 {
            self.flusher.flush(self.file.as_raw_fd(), &regions[..n])?;
        }
        self.flushed_head = head;
        self.dirty_seqs = None;
        self.header_dirty = false;
        self.index_dirty = false;
        Ok(())
    }

    fn header(&self) -> &Header {
        // SAFETY: the first page of the mapping, aligned, only accessed through self.
        unsafe { &*self.base.as_ptr().cast::<Header>() }
    }

    fn header_mut(&mut self) -> &mut Header {
        // SAFETY: as above, and &mut self makes it exclusive.
        unsafe { &mut *self.base.as_ptr().cast::<Header>() }
    }

    fn entry_ptr(&self, seq: u32) -> *mut Entry {
        let slot = (seq % self.entries) as usize;
        // SAFETY: slot < N; the index starts at the second page.
        unsafe { self.base.as_ptr().add(self.page + slot * ENTRY_SIZE).cast() }
    }

    fn data_ptr(&self) -> *mut u8 {
        // SAFETY: the data ring's first copy starts at data_off.
        unsafe { self.base.as_ptr().add(self.data_off) }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if self.policy != FlushPolicy::Never {
            let _ = self.flush();
        }
        // SAFETY: our own mapping. Closing the file then drops the flock.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.map_len) };
    }
}

/// "<our>-<their>.journal", with anything but [A-Za-z0-9._-] as '_'. The
/// header has the exact CompIDs.
fn file_name(our: &str, their: &str) -> String {
    let clean = |id: &str| id.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect::<String>();
    format!("{}-{}.journal", clean(our), clean(their))
}

fn page_size() -> usize {
    // SAFETY: plain sysconf.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page > 0 { page as usize } else { 4096 }
}

/// Reserves the file's blocks up front, so appends never hit ENOSPC through
/// the mapping (which would be a SIGBUS).
fn allocate(fd: libc::c_int, len: usize) -> io::Result<()> {
    // SAFETY: plain syscalls on our own fd.
    unsafe {
        if libc::fallocate(fd, 0, 0, len as libc::off_t) == 0 {
            return Ok(())
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EOPNOTSUPP) {
            return Err(e)
        }
        // A file system without fallocate: a sparse file will have to do.
        if libc::ftruncate(fd, len as libc::off_t) != 0 {
            return Err(io::Error::last_os_error())
        }
    }
    Ok(())
}

/// Maps header + index + data, then the data ring a second time right after
/// it. Shared, so stores go straight to the page cache; pre-faulted.
unsafe fn map(fd: libc::c_int, data_off: usize, capacity: usize) -> io::Result<NonNull<u8>> {
    let total = data_off + 2 * capacity;
    unsafe {
        let base = libc::mmap(ptr::null_mut(), total, libc::PROT_NONE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error())
        }
        let flags = libc::MAP_SHARED | libc::MAP_FIXED | libc::MAP_POPULATE;
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let parts = [(0, 0, data_off + capacity), (data_off + capacity, data_off, capacity)];
        for (at, file_offset, len) in parts {
            let addr = base.cast::<u8>().add(at).cast();
            if libc::mmap(addr, len, prot, flags, fd, file_offset as libc::off_t) != addr {
                let e = io::Error::last_os_error();
                libc::munmap(base, total);
                return Err(e)
            }
        }
        Ok(NonNull::new_unchecked(base.cast()))
    }
}

/// A fresh directory under the system temp dir, removed on drop.
#[cfg(test)]
pub(crate) struct TestDir(pub PathBuf);

#[cfg(test)]
impl TestDir {
    pub fn new(name: &str) -> Self {
        use std::sync::atomic::AtomicU32;
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fix-journal-{}-{name}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const T0: i64 = 1_792_947_600_000_000_000;

    fn config(dir: &TestDir, entries: u32, capacity: usize, flush: FlushPolicy) -> JournalConfig {
        JournalConfig { dir: dir.0.clone(), entries, capacity, flush, lock: false }
    }

    /// A message shaped like the session's: header, then `body`, then a trailer.
    fn message(seq: u32, body: &str) -> (Vec<u8>, usize) {
        let head = format!("8=FIX.4.4\x019=99\x0135=8\x0149=GATEWAY\x0156=CLIENT\x0134={seq}\x0152=20261025-17:00:00.000\x01");
        let msg = format!("{head}{body}10=000\x01").into_bytes();
        (msg, head.len())
    }

    /// Records every region it's asked to flush.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Vec<(u64, usize)>>>>);

    impl Flusher for Recorder {
        fn flush(&mut self, _fd: std::os::fd::RawFd, regions: &[Region<'_>]) -> io::Result<()> {
            self.0.lock().unwrap().push(regions.iter().map(|r| (r.file_offset, r.bytes.len())).collect());
            Ok(())
        }
    }

    impl Recorder {
        fn calls(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    #[test]
    fn append_and_get() {
        let dir = TestDir::new("append");
        let mut j = Journal::open(&config(&dir, 16, 4096, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).unwrap();
        assert_eq!((j.next_in_seq(), j.next_out_seq(), j.created_ns()), (1, 1, T0));
        let (msg, body_start) = message(2, "11=ord-1\x0137=O1\x01");
        j.append(2, T0 + 5, &msg, body_start);
        let stored = j.get(2).unwrap();
        assert_eq!((stored.seq, stored.sending_time, stored.bytes), (2, T0 + 5, &msg[..]));
        assert_eq!(stored.msg_type(), Some(&b"8"[..]));
        assert_eq!(stored.body(), b"11=ord-1\x0137=O1\x01");
        // Not stored, or empty slot.
        assert!(j.get(1).is_none() && j.get(3).is_none() && j.get(0).is_none());
        // Slot 18 % 16 = 2 holds seq 2, not 18.
        assert!(j.get(18).is_none());
    }

    #[test]
    fn index_wraps_after_n_messages() {
        let dir = TestDir::new("index-wrap");
        let mut j = Journal::open(&config(&dir, 8, 64 * 1024, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).unwrap();
        for seq in 1..=20 {
            let (msg, at) = message(seq, &format!("11=ord-{seq}\x01"));
            j.append(seq, T0, &msg, at);
        }
        for seq in 1..=12 {
            assert!(j.get(seq).is_none(), "{seq} fell out of the window");
        }
        for seq in 13..=20 {
            assert_eq!(j.get(seq).unwrap().body(), format!("11=ord-{seq}\x01").as_bytes());
        }
    }

    #[test]
    fn data_ring_wraps_and_stays_contiguous() {
        let dir = TestDir::new("data-wrap");
        let page = page_size();
        let mut j = Journal::open(&config(&dir, 1024, page, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).unwrap();
        // ~150-byte messages: the ring holds about 27 of them.
        let mut wrapped = false;
        for seq in 1..=100u32 {
            let (msg, at) = message(seq, &format!("58={}\x01", "x".repeat(seq as usize % 50 + 40)));
            let before = j.header().data_head as usize;
            wrapped |= before % page + msg.len() > page;
            j.append(seq, T0, &msg, at);
            assert_eq!(j.get(seq).unwrap().bytes, &msg[..]);
        }
        assert!(wrapped, "some message crossed the physical end");
        let mut kept = 0;
        for seq in 1..=100 {
            if let Some(m) = j.get(seq) {
                kept += 1;
                assert_eq!(m.bytes, &message(seq, &format!("58={}\x01", "x".repeat(seq as usize % 50 + 40))).0[..]);
            }
        }
        // Overwritten data reads back as missing even though its slot is intact.
        assert!(j.get(1).is_none());
        assert!((20..40).contains(&kept), "kept {kept}");
    }

    #[test]
    fn oversized_or_malformed_messages_are_not_stored() {
        let dir = TestDir::new("oversized");
        let page = page_size();
        let mut j = Journal::open(&config(&dir, 16, page, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).unwrap();
        j.append(1, T0, &vec![b'x'; page + 1], 10);
        j.append(2, T0, b"8=FIX.4.4\x01", 9);
        j.append(0, T0, &message(0, "").0, 10);
        assert!(j.get(1).is_none() && j.get(2).is_none());
        assert_eq!(j.header().data_head, 0);
    }

    #[test]
    fn persists_across_reopen() {
        let dir = TestDir::new("reopen");
        let cfg = config(&dir, 64, 64 * 1024, FlushPolicy::Never);
        let (msg, at) = message(7, "11=ord-7\x01");
        {
            let mut j = Journal::open(&cfg, "GATEWAY", "CLIENT", T0).unwrap();
            j.append(7, T0 + 1, &msg, at);
            j.set_seqs(12, 8);
        }
        let mut j = Journal::open(&cfg, "GATEWAY", "CLIENT", T0 + 99).unwrap();
        assert_eq!((j.next_in_seq(), j.next_out_seq(), j.created_ns()), (12, 8, T0));
        let stored = j.get(7).unwrap();
        assert_eq!((stored.bytes, stored.sending_time), (&msg[..], T0 + 1));
        // Appends carry on after the old data.
        let (msg8, at8) = message(8, "11=ord-8\x01");
        j.append(8, T0, &msg8, at8);
        assert_eq!(j.get(7).unwrap().bytes, &msg[..]);
        assert_eq!(j.get(8).unwrap().bytes, &msg8[..]);

        // Reset forgets everything, and stays forgotten.
        j.reset(T0 + 100);
        assert!(j.get(7).is_none() && j.get(8).is_none());
        drop(j);
        let j = Journal::open(&cfg, "GATEWAY", "CLIENT", T0).unwrap();
        assert_eq!((j.next_in_seq(), j.next_out_seq(), j.created_ns()), (1, 1, T0 + 100));
        assert!(j.get(7).is_none());
    }

    #[test]
    fn refuses_a_second_opener_other_comp_ids_and_other_geometry() {
        let dir = TestDir::new("refuse");
        let cfg = config(&dir, 64, 64 * 1024, FlushPolicy::Never);
        let j = Journal::open(&cfg, "GATEWAY", "CLIENT", T0).unwrap();
        assert!(Journal::open(&cfg, "GATEWAY", "CLIENT", T0).is_err(), "locked");
        drop(j);
        assert!(Journal::open(&config(&dir, 128, 64 * 1024, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).is_err());
        // Same file name, different CompIDs.
        fs::rename(dir.0.join("GATEWAY-CLIENT.journal"), dir.0.join("GATEWAY-CLIENT_.journal")).unwrap();
        assert!(Journal::open(&cfg, "GATEWAY", "CLIENT?", T0).is_err());
        assert!(Journal::open(&config(&dir, 64, 5000, FlushPolicy::Never), "A", "B", T0).is_err());
        assert!(Journal::open(&cfg, "A", &"B".repeat(MAX_COMP_ID + 1), T0).is_err());
    }

    #[test]
    fn flush_never() {
        let dir = TestDir::new("never");
        let rec = Recorder::default();
        let mut j = Journal::open_with(&config(&dir, 64, 64 * 1024, FlushPolicy::Never), "G", "C", T0, Box::new(rec.clone())).unwrap();
        assert_eq!(rec.calls(), 1, "the new header");
        let (msg, at) = message(2, "11=a\x01");
        j.append(2, T0, &msg, at);
        j.set_seqs(2, 3);
        j.on_timer(T0 + 3_600_000_000_000).unwrap();
        drop(j);
        assert_eq!(rec.calls(), 1);
    }

    #[test]
    fn flush_every_message() {
        let dir = TestDir::new("every");
        let rec = Recorder::default();
        let mut j = Journal::open_with(&config(&dir, 64, 64 * 1024, FlushPolicy::EveryMessage), "G", "C", T0, Box::new(rec.clone())).unwrap();
        let (msg, at) = message(2, "11=a\x01");
        j.append(2, T0, &msg, at);
        j.set_seqs(1, 3);
        let page = page_size() as u64;
        let data_off = page + page; // 64 entries fit one page
        assert_eq!(rec.0.lock().unwrap()[1], [(0, page as usize), (page, page as usize), (data_off, page as usize)]);
        // Inbound only: the header.
        j.set_seqs(2, 3);
        assert_eq!(rec.0.lock().unwrap()[2], [(0, page as usize)]);
        // Nothing changed: no flush.
        j.set_seqs(2, 3);
        assert_eq!(rec.calls(), 3);
    }

    #[test]
    fn flush_on_timer() {
        let dir = TestDir::new("timer");
        let rec = Recorder::default();
        let policy = FlushPolicy::Timer(Duration::from_millis(100));
        let mut j = Journal::open_with(&config(&dir, 64, 64 * 1024, policy), "G", "C", T0, Box::new(rec.clone())).unwrap();
        let ms = 1_000_000;
        let (msg, at) = message(2, "11=a\x01");
        j.append(2, T0, &msg, at);
        j.set_seqs(1, 3);
        assert_eq!(rec.calls(), 1, "not on the send path");
        j.on_timer(T0 + 50 * ms).unwrap();
        assert_eq!(rec.calls(), 1, "too early");
        j.on_timer(T0 + 100 * ms).unwrap();
        assert_eq!(rec.calls(), 2);
        // Nothing new: no flush.
        j.on_timer(T0 + 300 * ms).unwrap();
        assert_eq!(rec.calls(), 2);
        j.set_seqs(2, 3);
        j.on_timer(T0 + 350 * ms).unwrap();
        assert_eq!(rec.calls(), 2);
        j.on_timer(T0 + 400 * ms).unwrap();
        assert_eq!(rec.calls(), 3);
        // Dropping flushes what's left.
        j.set_seqs(3, 3);
        drop(j);
        assert_eq!(rec.calls(), 4);
    }

    #[test]
    fn dirty_index_and_data_wrap_into_two_regions() {
        let dir = TestDir::new("regions");
        let rec = Recorder::default();
        let page = page_size();
        let entries = (2 * page / ENTRY_SIZE) as u32; // two index pages
        let mut j = Journal::open_with(&config(&dir, entries, page, FlushPolicy::Never), "G", "C", T0, Box::new(rec.clone())).unwrap();
        // Fill up to just before both ends, flush, then write across them.
        let mut seq = entries - 3;
        let (msg, at) = message(seq, &format!("58={}\x01", "x".repeat(page - 200)));
        j.append(seq, T0, &msg, at);
        j.flush().unwrap();
        for _ in 0..5 {
            seq += 1;
            let (msg, at) = message(seq, "58=y\x01");
            j.append(seq, T0, &msg, at);
        }
        j.flush().unwrap();
        let last = rec.0.lock().unwrap().last().unwrap().clone();
        let data_off = (page + 2 * page) as u64;
        assert_eq!(last, [
            (0, page),                                              // header
            (page as u64 + page as u64, page), (page as u64, page), // index: tail, then head
            (data_off, page), (data_off, page),                     // data: tail page, then head page
        ]);
    }

    #[test]
    fn msync_flusher_works() {
        let dir = TestDir::new("msync");
        let mut j = Journal::open(&config(&dir, 64, 64 * 1024, FlushPolicy::EveryMessage), "G", "C", T0).unwrap();
        let (msg, at) = message(2, "11=a\x01");
        j.append(2, T0, &msg, at);
        j.set_seqs(1, 3);
        j.on_timer(T0).unwrap();
        // What's on disk, read through the file rather than the mapping.
        let disk = fs::read(j.path()).unwrap();
        let data_off = 2 * page_size();
        assert_eq!(&disk[data_off..data_off + msg.len()], &msg[..]);
    }

    /// cargo test --release store::tests::bench_append -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_append() {
        let dir = TestDir::new("bench");
        let mut j = Journal::open(&config(&dir, 65_536, 32 << 20, FlushPolicy::Never), "GATEWAY", "CLIENT", T0).unwrap();
        for size in [128usize, 256, 512, 1024] {
            let (msg, at) = message(1, &format!("58={}\x01", "x".repeat(size - 80)));
            let n = 2_000_000u32;
            let start = std::time::Instant::now();
            for seq in 1..=n {
                j.append(seq, T0, std::hint::black_box(&msg), at);
                j.set_seqs(seq, seq + 1);
            }
            let ns = start.elapsed().as_nanos() as f64 / n as f64;
            println!("append + set_seqs, {} bytes: {ns:.1} ns", msg.len());
        }
    }
}
