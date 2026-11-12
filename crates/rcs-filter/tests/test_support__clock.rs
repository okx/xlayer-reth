//! Deterministic, injectable test clock that avoids hidden time sources.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::clock::Clock;

/// A manually-advanced clock for deterministic timeout tests.
#[derive(Debug)]
pub struct TestClock {
    now: AtomicU64,
}

impl TestClock {
    /// Creates a clock reading `start` Unix seconds.
    pub fn new(start: u64) -> Self {
        Self { now: AtomicU64::new(start) }
    }

    /// Advances the clock by `secs` seconds.
    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    /// Sets the clock to an absolute Unix-seconds value.
    pub fn set(&self, secs: u64) {
        self.now.store(secs, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_unix(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}
