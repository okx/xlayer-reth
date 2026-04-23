//! Pool-transaction type alias for XLayer.
//!
//! Delegates to upstream [`reth_optimism_txpool::OpPooledTransaction<Cons, Pooled>`]
//! parameterized directly on the op-alloy envelopes — the
//! EIP-8130 (0x7B) variant now lives inside [`OpTxEnvelope`] /
//! [`OpPooledTransaction`] themselves (see the `eip8130` patch
//! under `deps/optimism/rust/op-alloy/`), so no XLayer-local
//! wrapper is needed.
//!
//! The upstream generic already carries all the `PoolTransaction` /
//! `EthPoolTransaction` / `OpPooledTx` / `DataAvailabilitySized`
//! blanket impls we need. This alias exists so downstream crates
//! (pool builder, node binary) have one concrete symbol to import.

use op_alloy_consensus::{OpPooledTransaction, OpTxEnvelope};

/// The concrete pool-transaction type for XLayer.
///
/// Wrap / unwrap via the upstream [`reth_optimism_txpool::OpPooledTransaction::new`]
/// / [`reth_optimism_txpool::PoolTransaction::from_pooled`]
/// constructors — no XLayer-specific entry points because the
/// cached-field computation (estimated compressed size, encoded
/// length) is identical for the AA variant.
pub type XLayerPoolTransaction =
    reth_optimism_txpool::OpPooledTransaction<OpTxEnvelope, OpPooledTransaction>;

#[cfg(test)]
mod tests {
    use super::*;
    use reth_optimism_txpool::OpPooledTx;
    use reth_transaction_pool::{EthPoolTransaction, PoolTransaction};

    /// Compile-gate: `XLayerPoolTransaction` satisfies every pool
    /// trait the upstream `OpPoolBuilder` / `OpTransactionValidator`
    /// pipeline expects.
    #[test]
    fn xlayer_pool_transaction_satisfies_pool_traits() {
        fn assert_traits<T: PoolTransaction + EthPoolTransaction + OpPooledTx>() {}
        assert_traits::<XLayerPoolTransaction>();
    }

    /// Pin the consensus / pooled associated types so downstream
    /// generics matching on them don't silently drift.
    #[test]
    fn associated_types_pin_to_op_envelopes() {
        fn assert_same<A: 'static, B: 'static>() {
            assert_eq!(core::any::TypeId::of::<A>(), core::any::TypeId::of::<B>());
        }
        assert_same::<<XLayerPoolTransaction as PoolTransaction>::Consensus, OpTxEnvelope>();
        assert_same::<<XLayerPoolTransaction as PoolTransaction>::Pooled, OpPooledTransaction>();
    }
}
