

#[derive(Debug, Clone)]
pub struct JournalConfig {
    /// One file per session in here, named after both CompIDs.
    pub dir: PathBuf,
    /// Index slots (N): how many of our latest messages can be resent.
    pub entries: u32,
    /// Data ring bytes (C): a power of two and a multiple of the page size.
    pub capacity: usize,
    pub flush: FlushPolicy,
    /// mlock the whole mapping (needs RLIMIT_MEMLOCK or CAP_IPC_LOCK).
    pub lock: bool,
}

impl JournalConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            entries: 65_536,
            capacity: 32 << 20,
            flush: FlushPolicy::Timer(std::time::Duration::from_secs(1)),
            lock: false,
        }
    }
}