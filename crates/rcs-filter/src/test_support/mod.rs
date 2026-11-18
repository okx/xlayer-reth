//! Test support: golden fixtures, a deterministic clock, a log builder, and a hand-written
//! [`RcsClient`] double.
//!
//! This module is compiled in normal builds (not `#[cfg(test)]`) so integration tests and
//! downstream crates can reuse the doubles; it pulls in no extra dependencies.

mod clock;
pub mod golden;
pub mod log_builder;
mod mock;

pub use clock::TestClock;
pub use mock::MockRcsClient;
