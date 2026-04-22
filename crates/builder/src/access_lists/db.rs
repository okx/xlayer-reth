//! Flashblock Access List Builder Database wrapper.
//!
//! `FBALBuilderDb` wraps any `Database + DatabaseCommit` implementation, intercepting all
//! EVM reads and writes to build an EIP-7928 access list transparently.

use crate::access_lists::builder::FlashblockAccessListBuilder;
use tracing::error;

use revm::{
    bytecode::Bytecode,
    database_interface::{Database, DatabaseCommit},
    primitives::{Address, HashMap, StorageKey, StorageValue, B256},
    state::{Account, AccountInfo},
};

/// A revm `Database` + `DatabaseCommit` wrapper that intercepts EVM reads and writes
/// to build an EIP-7928 flashblock access list.
///
/// Wraps an underlying database transparently. On each `basic()` call, registers the
/// address as touched. On each `storage()` call, records the slot as read. On each
/// `commit()`, diffs the changeset against pre-state and records only actual mutations.
///
/// # Error Handling
///
/// `DatabaseCommit::commit()` returns `()`, so errors from `try_commit()` are stored
/// internally and surfaced when `finish()` is called.
pub struct FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// The underlying database.
    db: DB,
    /// Current transaction index within the block.
    index: u64,
    /// Accumulates access list entries.
    access_list: FlashblockAccessListBuilder,
    /// Stores the first error from `try_commit()` since `commit()` can't return errors.
    error: Option<<DB as Database>::Error>,
}

impl<DB> FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// Creates a new wrapper around the given database.
    pub fn new(db: DB) -> Self {
        Self { db, index: 0, access_list: FlashblockAccessListBuilder::new(), error: None }
    }

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &DB {
        &self.db
    }

    /// Returns a mutable reference to the underlying database.
    pub fn db_mut(&mut self) -> &mut DB {
        &mut self.db
    }

    /// Sets the current transaction index.
    pub fn set_index(&mut self, index: u64) {
        self.index = index;
    }

    /// Increments the current transaction index.
    pub fn inc_index(&mut self) {
        self.index += 1;
    }

    /// Consumes the wrapper and returns the accumulated access list builder,
    /// or an error if `commit()` failed.
    pub fn finish(self) -> Result<FlashblockAccessListBuilder, <DB as Database>::Error> {
        if let Some(err) = self.error {
            return Err(err);
        }
        Ok(self.access_list)
    }

    /// Core diff logic: processes committed state changes, comparing against pre-state
    /// to record only actual mutations.
    fn try_commit(
        &mut self,
        changes: HashMap<Address, Account>,
    ) -> Result<(), <DB as Database>::Error> {
        for (address, account) in &changes {
            let entry = self.access_list.changes.entry(*address).or_default();

            // Diff against pre-state to record only actual mutations.
            let prev = self.db.basic(*address)?;

            if let Some(prev) = prev {
                // Existing account: diff fields
                if prev.balance != account.info.balance {
                    entry.balance_changes.insert(self.index, account.info.balance);
                }
                if prev.nonce != account.info.nonce {
                    entry.nonce_changes.insert(self.index, account.info.nonce);
                }
                if prev.code_hash != account.info.code_hash {
                    // Code changed — fetch the new bytecode
                    let code = if let Some(ref code) = account.info.code {
                        code.clone()
                    } else {
                        self.db.code_by_hash(account.info.code_hash)?
                    };
                    entry.code_changes.insert(self.index, code);
                }
            } else {
                // New account: record non-default values
                if !account.info.balance.is_zero() {
                    entry.balance_changes.insert(self.index, account.info.balance);
                }
                if account.info.nonce != 0 {
                    entry.nonce_changes.insert(self.index, account.info.nonce);
                }
                if account.info.code_hash != revm::primitives::KECCAK_EMPTY {
                    let code = if let Some(ref code) = account.info.code {
                        code.clone()
                    } else {
                        self.db.code_by_hash(account.info.code_hash)?
                    };
                    entry.code_changes.insert(self.index, code);
                }
            }

            // Record storage changes: only where original_value != present_value.
            for (slot, slot_value) in &account.storage {
                if slot_value.original_value != slot_value.present_value {
                    entry
                        .storage_changes
                        .entry(*slot)
                        .or_default()
                        .insert(self.index, slot_value.present_value);
                }
            }
        }

        // Commit to underlying DB.
        self.db.commit(changes);
        Ok(())
    }
}

impl<DB> Database for FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    type Error = <DB as Database>::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Register address as touched in the access list.
        self.access_list.changes.entry(address).or_default();
        self.db.basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db.code_by_hash(code_hash)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        // Record storage read.
        self.access_list.changes.entry(address).or_default().storage_reads.insert(index);
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB> DatabaseCommit for FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
    <DB as Database>::Error: std::fmt::Display,
{
    fn commit(&mut self, changes: HashMap<Address, Account>) {
        if let Err(err) = self.try_commit(changes) {
            error!(error = %err, "FBALBuilderDb: failed to commit access list changes");
            self.error = Some(err);
        }
    }
}
