use std::io::{self, Read};
use std::ptr::{self, NonNull};

pub struct RecvBuffer<const N: usize> {
    base: NonNull<u8>,
    head: usize,
    tail: usize,
}

unsafe impl<const N: usize> Send for RecvBuffer<N> {}

impl<const N: usize> RecvBuffer<N> {
    const MASK: usize = N.wrapping_sub(1);
    const CHECK: () = assert!(N.is_power_of_two(), "RecvBuffer size must be a power of two");

    pub fn new() -> io::Result<Self> {
        let () = Self::CHECK;

        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 || !N.is_multiple_of(page as usize) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RecvBuffer size must be a multiple of the page size",
            ));
        }

        unsafe {
            // TODO: uuid for fd name
            let fd = libc::memfd_create(c"fix_recv_buffer".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let result = Self::map_mirrored(fd);
            libc::close(fd);
            let base = result?;

            Ok(Self { base, head: 0, tail: 0 })
        }
    }

    unsafe fn map_mirrored(fd: libc::c_int) -> io::Result<NonNull<u8>> {
        unsafe {
            if libc::ftruncate(fd, N as libc::off_t) != 0 {
                return Err(io::Error::last_os_error());
            }

            // Reserve 2N of address space so both halves land side by side.
            let base = libc::mmap(
                ptr::null_mut(),
                2 * N,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if base == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }

            for half in 0..2 {
                let addr = base.cast::<u8>().add(half * N).cast();
                let mapped = libc::mmap(
                    addr,
                    N,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_FIXED | libc::MAP_POPULATE,
                    fd,
                    0,
                );
                if mapped != addr {
                    let err = io::Error::last_os_error();
                    libc::munmap(base, 2 * N);
                    return Err(err);
                }
            }

            Ok(NonNull::new_unchecked(base.cast()))
        }
    }

    /// Locks both halves in RAM so the hot path never page-faults.
    /// Needs `RLIMIT_MEMLOCK` >= 2N, or `CAP_IPC_LOCK`.
    pub fn lock(&self) -> io::Result<()> {
        if unsafe { libc::mlock(self.base.as_ptr().cast(), 2 * N) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[inline(always)]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Bytes received but not yet consumed.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.tail - self.head
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.tail == self.head
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.len() == N
    }

    #[inline(always)]
    pub fn spare_len(&self) -> usize {
        N - self.len()
    }

    /// Unread bytes as one contiguous slice, even across the physical end.
    #[inline(always)]
    pub fn data(&self) -> &[u8] {
        let start = self.head & Self::MASK;
        // start < N and len <= N, so the slice stays inside the 2N mapping.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(start), self.len()) }
    }

    /// Free space to read into, as one contiguous slice.
    #[inline(always)]
    pub fn spare_mut(&mut self) -> &mut [u8] {
        let start = self.tail & Self::MASK;
        // start < N and spare <= N. The spare bytes map to physical offsets
        // [tail, head + N) mod N, which never overlap the unread ones.
        unsafe {
            std::slice::from_raw_parts_mut(self.base.as_ptr().add(start), self.spare_len())
        }
    }

    /// Marks `n` bytes written into `spare_mut()` as received.
    #[inline(always)]
    pub fn commit(&mut self, n: usize) {
        // TODO: replace assert
        assert!(n <= self.spare_len(), "commit past the free space");
        self.tail += n;
    }

    /// Drops `n` bytes from the front once a message (or garbage) is handled.
    #[inline(always)]
    pub fn consume(&mut self, n: usize) {
        // TODO: replace assert
        assert!(n <= self.len(), "consume past the received data");
        self.head += n;
    }

    /// One `read` straight into the free space. `WouldBlock` passes through
    /// for the busy-poll loop. Reading into a full buffer returns `Ok(0)`,
    /// so check `is_full()` before treating 0 as a disconnect.
    #[inline]
    pub fn read_from<R: Read>(&mut self, src: &mut R) -> io::Result<usize> {
        let n = src.read(self.spare_mut())?;
        self.commit(n);
        Ok(n)
    }
}

impl<const N: usize> Drop for RecvBuffer<N> {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), 2 * N);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 4096 * 4;

    #[test]
    fn halves_mirror_each_other() {
        let mut buf = RecvBuffer::<SIZE>::new().unwrap();
        buf.spare_mut()[0] = 0xAB;
        let second = unsafe { *buf.base.as_ptr().add(SIZE) };
        assert_eq!(second, 0xAB);
    }

    #[test]
    fn data_is_contiguous_across_the_physical_end() {
        let mut buf = RecvBuffer::<SIZE>::new().unwrap();
        // Move the cursors to 10 bytes before the physical end.
        buf.commit(SIZE - 10);
        buf.consume(SIZE - 10);

        let msg = b"8=FIX.4.4\x019=5\x0135=0\x0110=000\x01";
        buf.spare_mut()[..msg.len()].copy_from_slice(msg);
        buf.commit(msg.len());

        assert_eq!(buf.data(), msg);
        buf.consume(msg.len());
        assert!(buf.is_empty());
    }

    #[test]
    fn spare_shrinks_and_grows_with_data() {
        let mut buf = RecvBuffer::<SIZE>::new().unwrap();
        assert_eq!(buf.spare_len(), SIZE);
        buf.commit(100);
        assert_eq!(buf.spare_mut().len(), SIZE - 100);
        buf.consume(40);
        assert_eq!(buf.len(), 60);
        assert_eq!(buf.spare_len(), SIZE - 60);
        buf.commit(SIZE - 60);
        assert!(buf.is_full());
        assert!(buf.spare_mut().is_empty());
    }

    #[test]
    fn read_from_fills_and_wraps() {
        let mut buf = RecvBuffer::<SIZE>::new().unwrap();
        let payload: Vec<u8> = (0..SIZE as u32 * 3).map(|i| (i % 251) as u8).collect();
        let mut src = &payload[..];
        let mut out = Vec::new();
        loop {
            let n = buf.read_from(&mut src).unwrap();
            if n == 0 && buf.is_empty() {
                break;
            }
            // Consume in odd-sized pieces so the head lands everywhere.
            let take = buf.len().min(777);
            out.extend_from_slice(&buf.data()[..take]);
            buf.consume(take);
        }
        assert_eq!(out, payload);
    }

    #[test]
    #[should_panic]
    fn commit_past_spare_panics() {
        let mut buf = RecvBuffer::<SIZE>::new().unwrap();
        buf.commit(SIZE + 1);
    }

    #[test]
    fn rejects_size_not_page_multiple() {
        assert!(RecvBuffer::<1024>::new().is_err());
    }
}
