//! Command that generates a genesis file from an existing op-reth data directory.
//!
//! This implementation:
//! - Reads a template genesis file for the "config" and other header fields
//! - Iterates over all accounts in the database
//! - Exports account balances, nonces, storage, and bytecode
//! - Merges with any existing "alloc" entries from the template (template takes priority)
//! - Sets "legacyXLayerBlock" and "number" to (latest block + 1) from the database
//! - Writes the complete genesis file with the "alloc" field populated

use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, Bytes, B256, U256};
use clap::Parser;
use eyre::{Result, WrapErr};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{AccessRights, Environment, EnvironmentArgs};
use reth_db_api::{
    cursor::{DbCursorRO, DbDupCursorRO},
    tables,
    transaction::DbTx,
};
use reth_node_core::version::version_metadata;
use reth_optimism_chainspec::OpChainSpec;
use reth_provider::BlockNumReader;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tracing::{debug, info, warn};

// The keccak256 of empty bytes is the well-known value 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470.
const EMPTY_CODE_HASH: B256 =
    alloy_primitives::b256!("c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470");

/// Generates a genesis file from an existing op-reth data directory.
#[derive(Debug, Parser)]
pub struct GenGenesisCommand<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,

    /// The path to the template genesis file.
    ///
    /// The "config" field and other header fields (nonce, timestamp, extraData, gasLimit,
    /// difficulty, mixHash, coinbase) will be copied from this file.
    ///
    /// If the template contains an "alloc" field, those accounts will be preserved and
    /// take priority over accounts read from the database.
    #[arg(long = "template-genesis", value_name = "TEMPLATE_GENESIS", verbatim_doc_comment)]
    template_genesis: PathBuf,

    /// The path to write the generated genesis file.
    #[arg(long = "output", value_name = "OUTPUT", verbatim_doc_comment)]
    output_path: PathBuf,

    /// Batch size for progress reporting.
    #[arg(long, value_name = "BATCH_SIZE", default_value = "100000")]
    batch_size: u64,
}

impl<C: ChainSpecParser<ChainSpec = OpChainSpec>> GenGenesisCommand<C> {
    /// Execute `gen-genesis` command
    pub async fn execute<N>(self) -> Result<()>
    where
        N: reth_cli_commands::common::CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        info!(target: "reth::cli", "{} ({}) starting", version_metadata().name_client, version_metadata().short_version);
        info!(target: "reth::cli", "Generating genesis from database");
        info!(target: "reth::cli", "Template genesis: {}", self.template_genesis.display());
        info!(target: "reth::cli", "Output: {}", self.output_path.display());

        // Read the template genesis file
        let template_genesis: Genesis = {
            let file = File::open(&self.template_genesis).wrap_err_with(|| {
                format!("Failed to open template genesis file: {}", self.template_genesis.display())
            })?;
            let reader = BufReader::new(file);
            serde_json::from_reader(reader).wrap_err("Failed to parse template genesis JSON")?
        };

        info!(target: "reth::cli", "Loaded template genesis with chain ID: {:?}", template_genesis.config.chain_id);

        // Check if template has existing alloc entries
        let template_alloc = template_genesis.alloc.clone();
        if !template_alloc.is_empty() {
            info!(
                target: "reth::cli",
                "Template genesis contains {} accounts (these will take priority)",
                template_alloc.len()
            );
        }

        // Setup interrupt handler before opening the DB so Ctrl+C is caught immediately
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        ctrlc::set_handler(move || {
            warn!(target: "reth::cli", "Received interrupt signal, shutting down gracefully...");
            shutdown_clone.store(true, Ordering::SeqCst);
        })
        .wrap_err("Failed to set interrupt handler")?;

        // Initialize the environment (opens the database in read-only mode)
        let Environment { provider_factory, .. } = self.env.init::<N>(AccessRights::RO)?;

        // Get the latest block number from the database
        let provider = provider_factory.provider()?;
        let latest_block =
            provider.last_block_number().wrap_err("Failed to get latest block number")?;

        // Genesis block number is latest + 1 (the next block after the exported state)
        let genesis_block_number = latest_block + 1;

        info!(
            target: "reth::cli",
            "Latest block number in database: {}",
            latest_block
        );
        info!(
            target: "reth::cli",
            "Genesis block number (latest + 1): {}",
            genesis_block_number
        );

        // Read all accounts from the database
        let tx = provider.tx_ref();

        let mut alloc = self.read_all_accounts(tx, &shutdown)?;

        if shutdown.load(Ordering::SeqCst) {
            return Err(eyre::eyre!("Genesis generation was interrupted"));
        }

        info!(target: "reth::cli", "Read {} accounts from database", alloc.len());

        // Merge template alloc entries (template takes priority over database)
        let mut overridden_count = 0usize;
        for (address, account) in template_alloc {
            if alloc.contains_key(&address) {
                overridden_count += 1;
            }
            alloc.insert(address, account);
        }

        if overridden_count > 0 {
            info!(
                target: "reth::cli",
                "Template alloc overrode {} accounts from database",
                overridden_count
            );
        }

        // Update the config with legacyXLayerBlock set to genesis block number (latest + 1)
        let mut config = template_genesis.config;
        config.extra_fields.insert(
            "legacyXLayerBlock".to_string(),
            serde_json::Value::Number(serde_json::Number::from(genesis_block_number)),
        );

        info!(
            target: "reth::cli",
            "Set legacyXLayerBlock to {} in genesis config",
            genesis_block_number
        );

        // Create the new genesis with the template config and the accounts from the database
        let new_genesis = Genesis {
            config,
            nonce: template_genesis.nonce,
            timestamp: template_genesis.timestamp,
            extra_data: template_genesis.extra_data,
            gas_limit: template_genesis.gas_limit,
            difficulty: template_genesis.difficulty,
            mix_hash: template_genesis.mix_hash,
            coinbase: template_genesis.coinbase,
            alloc,
            // Set number to genesis block number (latest + 1)
            number: Some(genesis_block_number),
            parent_hash: template_genesis.parent_hash,
            base_fee_per_gas: template_genesis.base_fee_per_gas,
            excess_blob_gas: template_genesis.excess_blob_gas,
            blob_gas_used: template_genesis.blob_gas_used,
        };

        // Write the genesis file
        let output_file = File::create(&self.output_path).wrap_err_with(|| {
            format!("Failed to create output file: {}", self.output_path.display())
        })?;
        let mut writer = BufWriter::new(output_file);

        serde_json::to_writer_pretty(&mut writer, &new_genesis)
            .wrap_err("Failed to write genesis JSON")?;
        writer.flush().wrap_err("Failed to flush output file")?;

        info!(
            target: "reth::cli",
            "Genesis generation complete! Wrote {} accounts to {}",
            new_genesis.alloc.len(),
            self.output_path.display()
        );

        Ok(())
    }

    /// Read all accounts from the database
    fn read_all_accounts<TX: DbTx>(
        &self,
        tx: &TX,
        shutdown: &AtomicBool,
    ) -> Result<BTreeMap<Address, GenesisAccount>> {
        let mut alloc = BTreeMap::new();
        let mut processed_accounts = 0u64;

        // Read all accounts from PlainAccountState
        info!(target: "reth::cli", "Reading accounts from database...");

        let mut account_cursor = tx.cursor_read::<tables::PlainAccountState>()?;

        // Walk through all accounts
        for result in account_cursor.walk(None)? {
            if shutdown.load(Ordering::SeqCst) {
                warn!(target: "reth::cli", "Interrupted after processing {} accounts", processed_accounts);
                return Err(eyre::eyre!("Interrupted"));
            }

            let (address, account) = result?;

            // Get storage for this account
            let storage = self.read_account_storage(tx, address)?;

            // Get bytecode if the account has code
            let code = if account.bytecode_hash.is_some() {
                self.read_account_bytecode(tx, account.bytecode_hash)?
            } else {
                None
            };

            // Create the genesis account
            let genesis_account = GenesisAccount {
                balance: account.balance,
                nonce: if account.nonce > 0 { Some(account.nonce) } else { None },
                code,
                storage: if storage.is_empty() { None } else { Some(storage) },
                private_key: None,
            };

            alloc.insert(address, genesis_account);
            processed_accounts += 1;

            // Log progress periodically
            if processed_accounts.is_multiple_of(self.batch_size) {
                info!(
                    target: "reth::cli",
                    "Processed {} accounts",
                    processed_accounts
                );
            }
        }

        debug!(target: "reth::cli", "Finished reading {} accounts", processed_accounts);

        Ok(alloc)
    }

    /// Read storage slots for an account
    fn read_account_storage<TX: DbTx>(
        &self,
        tx: &TX,
        address: Address,
    ) -> Result<BTreeMap<B256, B256>> {
        let mut storage = BTreeMap::new();
        let mut cursor = tx.cursor_dup_read::<tables::PlainStorageState>()?;

        // Walk all storage slots for this address
        if let Some(result) = cursor.seek_exact(address)? {
            let (_, entry) = result;
            // The first entry
            if entry.value != U256::ZERO {
                storage.insert(entry.key, B256::from(entry.value));
            }

            // Continue walking duplicates
            while let Some(result) = cursor.next_dup()? {
                let (_, entry) = result;
                if entry.value != U256::ZERO {
                    storage.insert(entry.key, B256::from(entry.value));
                }
            }
        }

        Ok(storage)
    }

    /// Read bytecode for an account
    fn read_account_bytecode<TX: DbTx>(
        &self,
        tx: &TX,
        bytecode_hash: Option<B256>,
    ) -> Result<Option<Bytes>> {
        let Some(hash) = bytecode_hash else {
            return Ok(None);
        };

        // Skip if it's the empty code hash (keccak256 of empty bytes)
        if hash == EMPTY_CODE_HASH {
            return Ok(None);
        }

        if let Some(bytecode) = tx.get::<tables::Bytecodes>(hash)? {
            Ok(Some(bytecode.original_bytes()))
        } else {
            warn!(target: "reth::cli", "Bytecode not found for hash {:?} - database may be corrupted", hash);
            Ok(None)
        }
    }
}
