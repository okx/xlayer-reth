//! Integration-test binary for the series' audit-RPC tests.
//!
//! Every batch case drives the complete production batch through the `test-utils` seam
//! `xlayer_rpc::audit::rpc::test_seams::execute_audit_batch` (or its foreign-stamp variant), so
//! request validation, rule compilation, deposit construction, the inspector choice, the
//! per-transaction loop and result assembly are all the shipped ones.

mod capture;
mod harness;
mod known_allowed;
mod replay;
mod reporting;
mod wire;
