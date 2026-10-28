mod all_stats;
mod helpers;
mod logs;
mod single;
mod types;

pub use all_stats::*;
pub use logs::*;
pub use single::*;
pub use types::*;

/// A cached disk-usage sample (bytes + when measured), or a cached list of
/// anonymous volume names (names set, bytes unused).
#[derive(Clone)]
pub struct UsageEntry {
    pub bytes: i64,
    pub at: std::time::Instant,
    pub names: Option<Vec<String>>,
}
