use std::time::Duration;

pub const BLOCK_SIZE: u32 = 1 << 14; // = 2^14 = 16KB
pub const MAX_ALLOWED_BLOCK_SIZE: u32 = 1 << 17; // = 2^17 = 128KB
pub const MAX_DOWNLOADERS: u32 = 3; // doesn't count optimistic unchoke
pub const ROLLING_AVERAGE_SIZE: usize = 20; // in secs
pub const TIMEOUT_DURATION: Duration = Duration::from_secs(5);
