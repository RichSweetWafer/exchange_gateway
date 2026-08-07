#[derive(Debug, PartialEq, Clone, Copy)]
pub enum FixVersion {
    // Fix42,
    // Fix43,
    Fix44,
}

impl FixVersion {
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            FixVersion::Fix44 => b"4.4",
        }
    }
}