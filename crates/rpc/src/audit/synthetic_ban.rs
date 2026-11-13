//! Re-export of the synthetic blacklist-replay derivation, which now lives in
//! `rcs_filter::audit::synthetic_ban` so the pure check sits next to the rest of the audit
//! adjudication logic. Kept as a pure re-export so `xlayer_rpc::audit::synthetic_ban::*` paths
//! are unchanged.

pub use rcs_filter::audit::synthetic_ban::*;
