//! Concrete [`XLayerAATxTr`] transaction wrapper.
//!
//! [`XLayerAATransaction`] layers [`XLayerAAParts`] over op-revm's
//! [`OpTransaction`]. For non-AA types the parts stay default (empty) and
//! all op-revm / mainnet behaviour is preserved.

use op_revm::transaction::{abstraction::OpTxTr, deposit::DEPOSIT_TRANSACTION_TYPE};
use revm::{
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
