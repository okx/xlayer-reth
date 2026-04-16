//! Type-erased handle to an [`Eip8130Pool`] for use in generic contexts.
//!
//! The payload builder's `FlashblocksBuilder<Pool, Client, Tasks>` stores the
//! pool as a generic `Pool` parameter with `#[derive(Debug, Clone)]`. Adding a
//! second generic for the AA pool would propagate through the entire builder
//! hierarchy.
//!
//! Instead, [`AaPoolHandle`] wraps a `SharedEip8130Pool<T>` behind
//! `Arc<dyn Any + Send + Sync>`, allowing it to be stored alongside the
//! generic `Pool` without introducing a new type parameter. At the call site
//! where the concrete pool transaction type is known, the handle is downcast
//! back to the concrete `Eip8130Pool<T>`.

use std::any::Any;
use std::sync::Arc;

use reth_transaction_pool::{BestTransactions, PoolTransaction, ValidPoolTransaction};
use tracing::warn;

use crate::merged::MergedBestTransactions;
use crate::pool::{BestEip8130Transactions, Eip8130Pool};

/// Type-erased handle to an [`Eip8130Pool`].
///
/// Implements `Clone` (via `Arc` refcount) and `Debug`, so it can be stored
/// in `#[derive(Debug, Clone)]` structs without adding a generic parameter.
#[derive(Clone)]
pub struct AaPoolHandle(Arc<dyn Any + Send + Sync>);

impl std::fmt::Debug for AaPoolHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AaPoolHandle").finish_non_exhaustive()
    }
}

impl AaPoolHandle {
    /// Wraps a shared AA pool in a type-erased handle.
    pub fn new<T: PoolTransaction + 'static>(pool: Arc<Eip8130Pool<T>>) -> Self {
        Self(pool)
    }

    /// Downcasts to the concrete `Eip8130Pool<T>`, returning `None` if `T`
    /// does not match the type originally stored.
    pub fn get<T: PoolTransaction + 'static>(&self) -> Option<&Eip8130Pool<T>> {
        self.0.downcast_ref::<Eip8130Pool<T>>()
    }

    /// Produces the AA side pool's best-transactions iterator, or `None` if
    /// the type parameter doesn't match.
    pub fn best_transactions<T: PoolTransaction + Clone + 'static>(
        &self,
    ) -> Option<BestEip8130Transactions<T>> {
        self.get::<T>().map(|pool| pool.best_transactions())
    }

    /// Wraps the given standard-pool best-transactions iterator with
    /// [`MergedBestTransactions`], interleaving AA transactions from this
    /// handle's pool.
    ///
    /// If the type parameter doesn't match (or the handle is absent), returns
    /// the standard iterator unchanged (boxed).
    pub fn merge_with_standard<T, S>(
        &self,
        standard: S,
    ) -> Box<dyn BestTransactions<Item = Arc<ValidPoolTransaction<T>>>>
    where
        T: PoolTransaction + Clone + 'static,
        S: BestTransactions<Item = Arc<ValidPoolTransaction<T>>> + 'static,
    {
        match self.best_transactions::<T>() {
            Some(aa_best) => Box::new(MergedBestTransactions::new(standard, aa_best)),
            None => {
                warn!(
                    target: "eip8130::pool",
                    "AaPoolHandle downcast failed: AA transactions will not be merged. \
                     Ensure the AA pool was created with the same concrete transaction type \
                     as Pool::Transaction."
                );
                Box::new(standard)
            }
        }
    }
}
