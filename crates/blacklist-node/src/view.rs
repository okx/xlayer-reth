//! FR-4 — live `MirrorViewCaller` adapter: read-only system-address staticcall.
//!
//! Implements the [`MirrorViewCaller`] seam from the core crate by issuing a read-only
//! system call (`caller = SystemAddress 0xff..fe`) to the mirror contract through an EVM
//! built over the parent state, and returning the raw ABI output. Any non-`Success`
//! result maps to [`ViewCallError::CallFailed`] (fail-open). The core
//! [`xlayer_blacklist::read_snapshot_at`] drives this with the `getBlacklist` pagination.
//!
//! Grounded signatures:
//! - `Evm::transact_system_call(&mut self, caller, contract, data) -> Result<ResultAndState, _>`
//!   — `deps/optimism/rust/alloy-op-evm/src/lib.rs:160` (the `alloy_evm::Evm` trait method,
//!   re-exported as `reth_evm::Evm`).
//! - `ExecutionResult::Success { output: Output, .. }` / `Output::into_data() -> Bytes`
//!   — revm `context::result` (`research-fr4-evm-inspector.md` §4).
//! - EVM construction over parent state: `State::builder().with_database(StateProviderDatabase)`
//!   — `crates/rpc/src/eth.rs:519`, `crates/builder/.../builder_tx.rs:427`.

use alloy_primitives::{Address, Bytes};
use reth_evm::Evm;
use revm::context::result::{ExecutionResult, ResultAndState};
use xlayer_blacklist::snapshot::SYSTEM_ADDRESS;
use xlayer_blacklist::{MirrorViewCaller, ViewCallError};

/// A [`MirrorViewCaller`] backed by a live EVM (`E: reth_evm::Evm`) built over the parent
/// state. Holds the EVM by mutable reference for the duration of a snapshot read.
pub struct RethMirrorViewCaller<'e, E> {
    evm: &'e mut E,
}

impl<'e, E> RethMirrorViewCaller<'e, E> {
    /// Wrap a mutable EVM reference. The caller is responsible for having built `evm` over
    /// the correct parent state with the block's `evm_env` (so `prevrandao`/`difficulty`
    /// obey the merge rule — see TD §4.1).
    pub fn new(evm: &'e mut E) -> Self {
        Self { evm }
    }
}

impl<E> MirrorViewCaller for RethMirrorViewCaller<'_, E>
where
    E: Evm,
{
    fn static_call(
        &mut self,
        to: Address,
        input: Bytes,
        _gas: u64,
    ) -> Result<Bytes, ViewCallError> {
        // Read-only system call from the system address; state changes are discarded.
        match self.evm.transact_system_call(SYSTEM_ADDRESS, to, input) {
            Ok(ResultAndState { result: ExecutionResult::Success { output, .. }, .. }) => {
                Ok(output.into_data())
            }
            // Revert / Halt / error / no code → fail-open (empty list at the caller).
            _ => Err(ViewCallError::CallFailed),
        }
    }
}
