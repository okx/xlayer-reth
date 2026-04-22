//! Pool-transaction type alias for XLayer.
//!
//! Delegates to upstream [`reth_optimism_txpool::OpPooledTransaction<Cons, Pooled>`]
//! parameterized on our own envelope types:
//!
//! - `Cons = XLayerTxEnvelope` — consensus form, accepted on-chain
//! - `Pooled = XLayerPooledTxEnvelope` — pool / P2P form
//!   (non-Deposit subset of consensus)
//!
//! The upstream generic already carries all the `PoolTransaction` /
//! `EthPoolTransaction` / `OpPooledTx` / `DataAvailabilitySized`
//! blanket impls we need. This alias exists so downstream crates
//! (pool builder, node binary) have one concrete symbol to import,
//! and so a future refactor of the pool tx's cached-field layout
//! only needs to update one line here.

use xlayer_consensus::{XLayerPooledTxEnvelope, XLayerTxEnvelope};

/// The concrete pool-transaction type for XLayer.
///
/// Wrap / unwrap via the upstream [`reth_optimism_txpool::OpPooledTransaction::new`]
/// / [`reth_optimism_txpool::PoolTransaction::from_pooled`]
/// constructors — no XLayer-specific entry points because the
/// cached-field computation (estimated compressed size, encoded
/// length) is identical for the AA variant.
pub type XLayerPoolTransaction =
    reth_optimism_txpool::OpPooledTransaction<XLayerTxEnvelope, XLayerPooledTxEnvelope>;

#[cfg(test)]
mod tests {
    use super::*;
    use reth_optimism_txpool::OpPooledTx;
    use reth_transaction_pool::{EthPoolTransaction, PoolTransaction};

    /// Compile-gate: `XLayerPoolTransaction` satisfies every pool
    /// trait the upstream `OpPoolBuilder` / `OpTransactionValidator`
    /// pipeline expects. If a future refactor drops one of the
    /// super-traits (say by renaming `OpPooledTx`), this test fails
    /// at compile time with a focused message rather than deep in
    /// the pool-builder's type-level plumbing.
    #[test]
    fn xlayer_pool_transaction_satisfies_pool_traits() {
        fn assert_traits<T: PoolTransaction + EthPoolTransaction + OpPooledTx>() {}
        assert_traits::<XLayerPoolTransaction>();
    }

    /// Pin the consensus / pooled associated types so downstream
    /// generics matching on them don't silently drift.
    #[test]
    fn associated_types_pin_to_xlayer_envelopes() {
        fn assert_same<A: 'static, B: 'static>() {
            assert_eq!(core::any::TypeId::of::<A>(), core::any::TypeId::of::<B>());
        }
        assert_same::<<XLayerPoolTransaction as PoolTransaction>::Consensus, XLayerTxEnvelope>();
        assert_same::<<XLayerPoolTransaction as PoolTransaction>::Pooled, XLayerPooledTxEnvelope>();
    }
}
