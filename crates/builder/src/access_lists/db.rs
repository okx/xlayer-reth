use alloy_primitives::{Address, B256};
use revm::{
    primitives::{HashMap, StorageKey, StorageValue, KECCAK_EMPTY},
    state::{Account, AccountInfo, Bytecode},
    Database, DatabaseCommit,
};
use std::sync::{Arc, Mutex};
use tracing::error;

use crate::access_lists::builder::FlashblockAccessListBuilder;

/// A [`Database`] implementation that builds an access list based on reads and writes.
/// Use `std::mem::take(&mut *lock.guard)` at flashblock-finalization time on the access
/// list builder reference to retrieve the accumulated builder.
#[derive(Debug)]
pub struct FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// Underlying `CacheDB`
    db: DB,
    /// Transaction index of the transaction currently being executed
    index: u64,
    /// Builder for the access list
    access_list: Arc<Mutex<FlashblockAccessListBuilder>>,
}

impl<DB> FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// Creates a new instance of [`FBALBuilderDb`] with the given underlying database
    pub fn new(db: DB, access_list: Arc<Mutex<FlashblockAccessListBuilder>>) -> Self {
        Self { db, index: 0, access_list }
    }

    /// Returns a reference to the underlying database
    pub const fn db(&self) -> &DB {
        &self.db
    }

    /// Returns a mutable reference to the underlying database
    pub const fn db_mut(&mut self) -> &mut DB {
        &mut self.db
    }

    /// Sets the transaction index of the transaction currently being executed
    pub const fn set_index(&mut self, index: u64) {
        self.index = index;
    }

    /// Increments the transaction index of the transaction currently being executed
    pub const fn inc_index(&mut self) {
        self.index += 1;
    }

    /// Attempts to apply account/storage changes to the account list builder
    fn update_access_list(
        &mut self,
        changes: &HashMap<Address, Account>,
    ) -> Result<(), <DB as Database>::Error> {
        for (address, account) in changes {
            let mut access_list = self.access_list.lock().expect("access list mutex poisoned");
            let account_changes = access_list.changes.entry(*address).or_default();

            // Update balance, nonce, and code
            match self.db.basic(*address)? {
                Some(prev) => {
                    if prev.balance != account.info.balance {
                        account_changes.balance_changes.insert(self.index, account.info.balance);
                    }

                    if prev.nonce != account.info.nonce {
                        account_changes.nonce_changes.insert(self.index, account.info.nonce);
                    }

                    if prev.code_hash != account.info.code_hash {
                        let bytecode = match account.info.code.clone() {
                            Some(code) => code,
                            None => self.db.code_by_hash(account.info.code_hash)?,
                        };
                        account_changes.code_changes.insert(self.index, bytecode);
                    }
                }
                None => {
                    // For new accounts, only record changes if they differ from defaults
                    if !account.info.balance.is_zero() {
                        account_changes.balance_changes.insert(self.index, account.info.balance);
                    }
                    if account.info.nonce != 0 {
                        account_changes.nonce_changes.insert(self.index, account.info.nonce);
                    }
                    // Only record code changes if the account actually has code
                    if account.info.code_hash != KECCAK_EMPTY {
                        let bytecode = match account.info.code.clone() {
                            Some(code) => code,
                            None => self.db.code_by_hash(account.info.code_hash)?,
                        };
                        account_changes.code_changes.insert(self.index, bytecode);
                    }
                }
            }

            // Update storage
            for (slot, value) in &account.storage {
                let prev = value.original_value;
                let new = value.present_value;

                if prev != new {
                    account_changes
                        .storage_changes
                        .entry(*slot)
                        .or_default()
                        .insert(self.index, new);
                }
            }
        }

        Ok(())
    }
}

impl<DB> Database for FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    type Error = <DB as Database>::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.access_list
            .lock()
            .expect("access list mutex poisoned")
            .changes
            .entry(address)
            .or_default();
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
        self.access_list
            .lock()
            .expect("access list mutex poisoned")
            .changes
            .entry(address)
            .or_default()
            .storage_reads
            .insert(index);
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB> DatabaseCommit for FBALBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    fn commit(&mut self, changes: HashMap<Address, Account>) {
        if let Err(e) = self.update_access_list(&changes) {
            error!(target: "payload_builder", error = ?e, "Failed to update FBAL access list");
        }
        self.db.commit(changes);
    }
}
