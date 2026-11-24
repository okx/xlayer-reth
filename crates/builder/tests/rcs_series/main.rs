//! The RCS series' builder loop-level tests, run as this crate's integration-test binary
//! `rcs_series`. Cases are grouped by behaviour and sit directly in this directory; builder
//! internals are reached only through the non-default `test-utils` seam
//! `xlayer_builder::flashblocks::test_seams`.

mod candidate_loop;
mod cross_path;
mod deposit_execution;
mod fixtures;
