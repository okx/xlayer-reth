//! Re-export of the shared capture crate.
//!
//! The inspector, its capture-control seam, the capture outcomes and the RAII capture guard now
//! live in `xlayer-rcs-capture`, so the audit-RPC path runs the **same** implementation instead
//! of a second one that could drift. This module stays in place as a pure re-export, so every
//! existing `use super::rcs_capture::..` path in the builder is unchanged.
//!
//! The builder's own EVM adapter for [`RcsCaptureScope`] lives in
//! [`super::context::BuilderCaptureScope`]: the shared crate owns the trait but names no EVM
//! type, so `impl RcsCaptureScope for OpEvm<..>` has to sit in the crate that owns a local type
//! wrapping it.

pub use xlayer_rcs_capture::*;
