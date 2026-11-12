//! Test support: golden fixtures, a deterministic clock, a log builder, and a hand-written
//! [`RcsClient`] double.
//!
//! This module is compiled in normal builds (not `#[cfg(test)]`) so integration tests and
//! downstream crates can reuse the doubles; it pulls in no extra dependencies.

#[path = "test_support__clock.rs"]
mod clock;
#[path = "test_support__golden.rs"]
pub mod golden;
#[path = "test_support__log_builder.rs"]
pub mod log_builder;
#[path = "test_support__mock.rs"]
mod mock;

pub use clock::TestClock;
pub use mock::MockRcsClient;
