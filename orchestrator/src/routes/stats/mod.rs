mod all_stats;
mod helpers;
mod logs;
mod single;
mod types;

pub use all_stats::*;
pub use logs::*;
pub use single::*;
pub use types::*;

/// A cached disk-usage sample (bytes + when measured).
#[derive(Clone, Copy)]
pub struct UsageEntry {
    pub bytes: i64,
    pub at: std::time::Instant,
}
