//! FR-4 — live `MirrorViewCaller` adapter: read-only 50M-gas call to the mirror.
//!
//! Implements the [`MirrorViewCaller`] seam from the core crate by issuing a read-only
//! call (`caller = SystemAddress 0xff..fe`, `gas_limit = PER_PAGE_GAS = 50M`) to the
//! mirror contract through an `OpEvm` built over the parent state, returning the raw ABI
//! output. This mirrors op-geth's `evm.StaticCall(SystemAddress, mirror, input, 50M)`
//! (decision B: guarantee the same 50M gas budget cross-client, not the 30M default of
//! `transact_system_call`). The call follows the in-repo simulation idiom
//! (`builder_tx.rs::simulate_call`): disable balance / block-gas-limit / nonce / base-fee
//! checks via `modify_cfg`, build an `OpTransactionRequest`, and discard the resulting
//! state (read-only). Any non-`Success` result maps to [`ViewCallError::CallFailed`]
//! (fail-open). The core [`xlayer_blacklist::read_snapshot_at`] drives this with the
//! `getBlacklist` pagination.

use alloy_evm::{rpc::TryIntoTxEnv, Database, Evm};
use alloy_op_evm::OpEvm;
use alloy_primitives::{Address, Bytes};
use alloy_rpc_types_eth::TransactionInput;
use op_alloy_rpc_types::OpTransactionRequest;
use reth_evm::precompiles::PrecompilesMap;
use reth_optimism_evm::OpTx;
use revm::{
    context::result::{ExecutionResult, ResultAndState},
    inspector::NoOpInspector,
};
use xlayer_blacklist::snapshot::SYSTEM_ADDRESS;
use xlayer_blacklist::{read_snapshot_at, BlacklistSnapshot, MirrorViewCaller, ViewCallError};

/// A [`MirrorViewCaller`] backed by an `OpEvm` built over the parent state. Holds the EVM
/// by mutable reference for the duration of a snapshot read. Concrete over `OpEvm` (not a
/// generic `Evm`) because constructing the 50M-gas call requires the concrete `OpTx`
/// transaction-env type.
pub struct RethMirrorViewCaller<'e, DB: Database + core::fmt::Debug> {
    evm: &'e mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>,
}

impl<'e, DB> RethMirrorViewCaller<'e, DB>
where
    DB: Database + core::fmt::Debug,
{
    /// Wrap a mutable `OpEvm` reference, disabling the checks that a read-only system-style
    /// call must not be subject to (the system address holds no funds; the 50M budget may
    /// exceed the block gas limit; nonce / base-fee are irrelevant for a view read). The
    /// caller is responsible for having built `evm` over the correct parent state with the
    /// block's `evm_env` (so `prevrandao`/`difficulty` obey the merge rule — see TD §4.1).
    pub fn new(evm: &'e mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>) -> Self {
        evm.modify_cfg(|cfg| {
            cfg.disable_balance_check = true;
            cfg.disable_block_gas_limit = true;
            cfg.disable_nonce_check = true;
            cfg.disable_base_fee = true;
        });
        Self { evm }
    }
}

impl<DB> MirrorViewCaller for RethMirrorViewCaller<'_, DB>
where
    DB: Database + core::fmt::Debug,
{
    fn static_call(&mut self, to: Address, input: Bytes, gas: u64) -> Result<Bytes, ViewCallError> {
        // Read-only call from the system address with the cross-client 50M gas budget.
        let tx_req = OpTransactionRequest::default()
            .gas_limit(gas)
            .max_fee_per_gas(0)
            .to(to)
            .from(SYSTEM_ADDRESS)
            .input(TransactionInput::new(input));

        let evm_env = alloy_evm::EvmEnv::from((self.evm.cfg.clone(), self.evm.block.clone()));
        let base = tx_req
            .as_ref()
            .clone()
            .try_into_tx_env(&evm_env)
            .map_err(|_| ViewCallError::CallFailed)?;
        let tx_env = OpTx(op_revm::OpTransaction {
            base,
            enveloped_tx: Some(Bytes::new()),
            deposit: Default::default(),
        });

        // Discard the returned state (read-only); only the output bytes matter.
        match self.evm.transact(tx_env) {
            Ok(ResultAndState { result: ExecutionResult::Success { output, .. }, .. }) => {
                Ok(output.into_data())
            }
            // Revert / Halt / error / no code → fail-open (empty list at the caller).
            _ => Err(ViewCallError::CallFailed),
        }
    }
}

/// Read the full block-head blacklist snapshot from `mirror` via the given read-only
/// `OpEvm` (built over the parent state), using the cross-client `getBlacklist`
/// pagination ([`read_snapshot_at`]). Any view-call failure fails open to an empty
/// snapshot. The caller is responsible for building `evm` over the correct parent state.
pub fn read_blacklist_snapshot<DB: Database + core::fmt::Debug>(
    evm: &mut OpEvm<DB, NoOpInspector, PrecompilesMap, OpTx>,
    mirror: Address,
) -> BlacklistSnapshot {
    let mut caller = RethMirrorViewCaller::new(evm);
    read_snapshot_at(&mut caller, mirror)
}
