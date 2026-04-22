//! Concrete [`XLayerAATxTr`] transaction wrapper.
//!
//! [`XLayerAATransaction`] layers [`XLayerAAParts`] over op-revm's
//! [`OpTransaction`]. For non-AA types the parts stay default (empty) and
//! all op-revm / mainnet behaviour is preserved.

use alloy_evm::{FromRecoveredTx, FromTxWithEncoded, IntoTxEnv};
use op_alloy_consensus::OpTxEnvelope;
use op_revm::transaction::{abstraction::OpTxTr, deposit::DEPOSIT_TRANSACTION_TYPE};
use reth_optimism_evm::OpTx;
use revm::{
    context::TxEnv as RevmTxEnv,
    context_interface::Transaction,
    handler::SystemCallTx,
    primitives::{Address, Bytes, TxKind, B256, U256},
};

use crate::{
    constants::XLAYERAA_TX_TYPE,
    transaction::{XLayerAAParts, XLayerAATxTr},
};

/// XLayerAA-aware transaction: op-stack deposit/regular fields + optional
/// `XLayerAAParts` for type `0x7B`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XLayerAATransaction<T: Transaction> {
    /// Underlying op-revm transaction (deposit + base fields).
    pub op: op_revm::OpTransaction<T>,
    /// XLayerAA execution parts — default (empty) for non-AA tx types.
    pub xlayeraa: XLayerAAParts,
}

impl<T: Transaction> XLayerAATransaction<T> {
    /// Wrap an op-revm transaction with default (empty) AA parts.
    pub fn new(op: op_revm::OpTransaction<T>) -> Self {
        Self { op, xlayeraa: XLayerAAParts::default() }
    }

    /// Wrap an op-revm transaction with the given AA parts.
    pub fn with_parts(op: op_revm::OpTransaction<T>, xlayeraa: XLayerAAParts) -> Self {
        Self { op, xlayeraa }
    }
}

impl<T: Transaction + Default> Default for XLayerAATransaction<T>
where
    op_revm::OpTransaction<T>: Default,
{
    fn default() -> Self {
        Self::new(op_revm::OpTransaction::<T>::default())
    }
}

impl<T: Transaction + SystemCallTx> SystemCallTx for XLayerAATransaction<T> {
    fn new_system_tx_with_caller(
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> Self {
        Self::new(op_revm::OpTransaction::<T>::new_system_tx_with_caller(
            caller,
            system_contract_address,
            data,
        ))
    }
}

// The underlying `op` already dispatches `tx_type()` between deposit and
// `base.tx_type()` so AA txs just need `base.tx_type() == 0x7B`.
impl<T: Transaction> Transaction for XLayerAATransaction<T> {
    type AccessListItem<'a>
        = <op_revm::OpTransaction<T> as Transaction>::AccessListItem<'a>
    where
        T: 'a;
    type Authorization<'a>
        = <op_revm::OpTransaction<T> as Transaction>::Authorization<'a>
    where
        T: 'a;

    fn tx_type(&self) -> u8 {
        self.op.tx_type()
    }
    fn caller(&self) -> Address {
        self.op.caller()
    }
    fn gas_limit(&self) -> u64 {
        self.op.gas_limit()
    }
    fn value(&self) -> U256 {
        self.op.value()
    }
    fn input(&self) -> &Bytes {
        self.op.input()
    }
    fn nonce(&self) -> u64 {
        self.op.nonce()
    }
    fn kind(&self) -> TxKind {
        self.op.kind()
    }
    fn chain_id(&self) -> Option<u64> {
        self.op.chain_id()
    }
    fn access_list(&self) -> Option<impl Iterator<Item = Self::AccessListItem<'_>>> {
        self.op.access_list()
    }
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.op.max_priority_fee_per_gas()
    }
    fn max_fee_per_gas(&self) -> u128 {
        self.op.max_fee_per_gas()
    }
    fn gas_price(&self) -> u128 {
        self.op.gas_price()
    }
    fn blob_versioned_hashes(&self) -> &[B256] {
        self.op.blob_versioned_hashes()
    }
    fn max_fee_per_blob_gas(&self) -> u128 {
        self.op.max_fee_per_blob_gas()
    }
    fn effective_gas_price(&self, base_fee: u128) -> u128 {
        self.op.effective_gas_price(base_fee)
    }
    fn authorization_list_len(&self) -> usize {
        self.op.authorization_list_len()
    }
    fn authorization_list(&self) -> impl Iterator<Item = Self::Authorization<'_>> {
        self.op.authorization_list()
    }
}

impl<T: Transaction> OpTxTr for XLayerAATransaction<T> {
    fn enveloped_tx(&self) -> Option<&Bytes> {
        self.op.enveloped_tx()
    }
    fn source_hash(&self) -> Option<B256> {
        self.op.source_hash()
    }
    fn mint(&self) -> Option<u128> {
        self.op.mint()
    }
    fn is_system_transaction(&self) -> bool {
        self.op.is_system_transaction()
    }
}

impl<T: Transaction> XLayerAATxTr for XLayerAATransaction<T> {
    fn xlayeraa_parts(&self) -> &XLayerAAParts {
        &self.xlayeraa
    }
}

/// Deposit tx-type byte (re-export from op-revm).
pub const XLAYERAA_TX_TYPE_DEPOSIT: u8 = DEPOSIT_TRANSACTION_TYPE;
/// Convenience re-export of the XLayerAA tx-type byte.
pub const XLAYERAA_TX_TYPE_AA: u8 = XLAYERAA_TX_TYPE;

// Identity conversion so `XLayerAATransaction<T>` can be fed to
// [`alloy_evm::EvmFactory::create_evm`] directly (the factory requires
// `Self::Tx: IntoTxEnv<Self::Tx>`). Mirrors op-revm's blanket
// `IntoTxEnv<Self> for OpTransaction<T>`.
impl<T: Transaction> IntoTxEnv<Self> for XLayerAATransaction<T> {
    fn into_tx_env(self) -> Self {
        self
    }
}

// The OP block executor factory surfaces the enveloped tx bytes via its
// own `OpTxEnv` trait (used for L1 cost accounting). Delegate to the
// inner `OpTransaction` so non-AA txs keep their canonical encoded bytes.
impl<T: Transaction> alloy_op_evm::block::OpTxEnv for XLayerAATransaction<T> {
    fn encoded_bytes(&self) -> Option<&Bytes> {
        self.op.encoded_bytes()
    }
}

// Reth's `ConfigureEvm` requires `Self::Tx: TransactionEnv` — a cluster of
// set_* helpers used by the payload builder to adjust gas / nonce / access
// list on pooled txs before execution. Delegate through the inner OP tx.
impl<T> reth_evm::TransactionEnv for XLayerAATransaction<T>
where
    T: reth_evm::TransactionEnv + Transaction,
{
    fn set_gas_limit(&mut self, gas_limit: u64) {
        self.op.set_gas_limit(gas_limit);
    }
    fn nonce(&self) -> u64 {
        reth_evm::TransactionEnv::nonce(&self.op)
    }
    fn set_nonce(&mut self, nonce: u64) {
        self.op.set_nonce(nonce);
    }
    fn set_access_list(&mut self, access_list: revm::context::transaction::AccessList) {
        self.op.set_access_list(access_list);
    }
}

// --- Wire → exec conversions for the OP tx envelope -----------------------
//
// The pool / payload builder hand pooled transactions to the EVM factory as
// `OpTxEnvelope` (Legacy / 2930 / 1559 / 7702 / Deposit). We delegate the
// non-AA shapes to `OpTx` (op-reth's newtype over `OpTransaction<TxEnv>`)
// and wrap the result with empty `XLayerAAParts` — 0x7B transactions ride
// on a separate extended envelope that lands in a follow-up milestone.

impl FromRecoveredTx<OpTxEnvelope> for XLayerAATransaction<RevmTxEnv> {
    fn from_recovered_tx(tx: &OpTxEnvelope, sender: Address) -> Self {
        Self::new(OpTx::from_recovered_tx(tx, sender).into())
    }
}

impl FromTxWithEncoded<OpTxEnvelope> for XLayerAATransaction<RevmTxEnv> {
    fn from_encoded_tx(tx: &OpTxEnvelope, caller: Address, encoded: Bytes) -> Self {
        Self::new(OpTx::from_encoded_tx(tx, caller, encoded).into())
    }
}

// --- RPC request → exec env conversion -----------------------------------
//
// `eth_call` / `eth_estimateGas` / `eth_sendTransaction` hand the RPC
// layer an [`OpTransactionRequest`], which reth's `RpcConvert` plumbing
// funnels through [`alloy_evm::rpc::TryIntoTxEnv`] into the EVM's tx
// type. Upstream `alloy-evm` only provides the impl for
// [`op_revm::OpTransaction<TxEnv>`]; our [`XLayerAATransaction<TxEnv>`]
// (the `EvmFactory::Tx` on [`XLayerAAEvmFactory`](crate::XLayerAAEvmFactory))
// needs its own, so `OpEthApiBuilder` can satisfy
// `EthApiBuilder<Node>` for nodes wired with `XLayerEvmConfig`.
//
// RPC-origin requests carry no AA parts (the XLayerAA body is a
// separate envelope landing in a later milestone), so we forward to the
// upstream `OpTransaction<TxEnv>` impl and wrap with default
// `XLayerAAParts`. The `rpc` feature gates the pulled-in
// `op-alloy-rpc-types` dep so no_std / prover consumers stay lean.
#[cfg(feature = "rpc")]
mod rpc_try_into {
    use super::*;
    use alloy_evm::{
        env::BlockEnvironment,
        rpc::{EthTxEnvError, TryIntoTxEnv},
        EvmEnv,
    };
    use op_alloy_rpc_types::OpTransactionRequest;

    impl<Block: BlockEnvironment> TryIntoTxEnv<XLayerAATransaction<RevmTxEnv>, Block>
        for OpTransactionRequest
    {
        type Err = EthTxEnvError;

        fn try_into_tx_env<Spec>(
            self,
            evm_env: &EvmEnv<Spec, Block>,
        ) -> Result<XLayerAATransaction<RevmTxEnv>, Self::Err> {
            let op_tx: op_revm::OpTransaction<RevmTxEnv> = self.try_into_tx_env(evm_env)?;
            Ok(XLayerAATransaction::new(op_tx))
        }
    }
}
