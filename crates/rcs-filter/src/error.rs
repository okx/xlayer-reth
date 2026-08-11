//! Typed `thiserror` errors for the RCS Filter library.

use thiserror::Error;

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, FilterError>;

/// All fallible operations in the filter surface one of these typed errors so callers
/// (the background worker, the startup loader) can branch on the failure mode.
#[derive(Debug, Error)]
pub enum FilterError {
    /// Transport-level failure talking to RCS (connection refused, timeout, TLS, …).
    #[error("rcs transport error: {0}")]
    Transport(String),

    /// The configured connect or total-request deadline elapsed.
    #[error("rcs request timed out: {0}")]
    Timeout(String),

    /// RCS responded with an unexpected (non-2xx / non-202) HTTP status.
    #[error("rcs unexpected status: {0}")]
    UnexpectedStatus(u16),

    /// Response body could not be decoded into the expected schema.
    #[error("rcs decode error: {0}")]
    Decode(String),

    /// `protocol_version` returned by RCS is outside the locally supported set
    /// (see [`crate::config::SUPPORTED_PROTOCOL_VERSIONS`]).
    #[error("unsupported protocol_version: {0}")]
    UnsupportedProtocol(u32),

    /// Configuration is invalid (e.g. `enabled=true` without an `rcs_base_url`).
    #[error("filter configuration error: {0}")]
    Config(String),

    /// One or more rules in an otherwise well-formed `/rules` response failed per-rule
    /// validation (e.g. empty `event_abis`, duplicate id, invalid JSONLogic). Surfaced as
    /// an error — rather than silently installing the valid subset — so a partially-invalid
    /// batch never replaces the currently-active rule set.
    #[error("rule set contains {0} invalid rule(s), rejecting entire update: {1:?}")]
    InvalidRules(usize, Vec<crate::rules::RejectedRule>),
}
