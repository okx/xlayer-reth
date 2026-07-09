//! Injectable clock (TD prohibition: no hidden time sources — inject a `Clock` trait and
//! mock it in tests). Time is expressed as Unix seconds, matching the RCS `decided_at`
//! and the golden-fixture timeline (contract §4).

use std::fmt::Debug;

/// Abstraction over "current wall-clock time in Unix seconds" so timeout logic (FR-6) is
/// deterministically testable via [`crate::test_support::TestClock`].
pub trait Clock: Send + Sync + Debug {
    /// Current time in Unix seconds.
    fn now_unix(&self) -> u64;
}

/// Production clock backed by the system wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
