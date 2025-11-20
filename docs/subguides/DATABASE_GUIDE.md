# Database & Storage Guide

This guide explains Reth's hybrid database architecture combining MDBX (mutable storage) and static files (immutable storage), and how to work with both systems.

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [Database Traits](#database-traits)
4. [Working with Tables](#working-with-tables)
5. [MDBX Database](#mdbx-database)
6. [Static Files](#static-files)
7. [Provider Pattern](#provider-pattern)
8. [Reading and Writing Data](#reading-and-writing-data)
9. [Custom Tables](#custom-tables)

## Overview

Reth uses a **hybrid storage approach** that separates hot and cold data for optimal performance:

- **MDBX (Hot Data)**: Recent mutable blockchain state and history
- **Static Files (Cold Data)**: Finalized immutable historical data

### Why This Architecture?

```
┌─────────────────────────────────────┐
│         Query Pattern               │
│                                     │
│  Recent Blocks → MDBX (Random R/W) │
│  Old Blocks → Static Files (Seq R) │
└─────────────────────────────────────┘

Benefits:
✓ Fast writes for recent data (MDBX)
✓ Efficient reads for historical data (Static Files)
✓ Lower memory footprint (compressed archives)
✓ Reduced database growth (pruning old MDBX data)
```

### Data Flow

```
                    ┌──────────────┐
                    │  RPC Request │
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │   Provider   │
                    └──────┬───────┘
                           │
         ┌─────────────────┴─────────────────┐
         │                                   │
    Block > highest_static_file_block?      │
         │                                   │
    ┌────▼────┐                       ┌─────▼──────┐
    │  MDBX   │                       │Static Files│
    │  (Hot)  │                       │   (Cold)   │
    └─────────┘                       └────────────┘
```

## Architecture

### Core Components

| Component | Location | Purpose |
|-----------|----------|---------|
| **Database Trait** | `crates/storage/db-api/` | Abstract database interface |
| **MDBX Implementation** | `crates/storage/db/` | Concrete MDBX database |
| **Static Files** | `crates/storage/static-file/` | Immutable data archives |
| **Providers** | `crates/storage/provider/` | Business logic layer |
| **Tables** | `crates/storage/db-api/src/tables/` | Table definitions |

### Data Lifecycle

```
1. New Block Received
   ↓
2. Written to MDBX (mutable)
   ↓
3. Block Finalized (after ~8192 blocks)
   ↓
4. Copied to Static Files (every 500K blocks)
   ↓
5. Pruned from MDBX (optional)
```

## Database Traits

### 1. Database - Main Interface

**Location**: `crates/storage/db-api/src/database.rs`

```rust
pub trait Database: Send + Sync {
    /// Read-only transaction type
    type TX: DbTx;

    /// Read-write transaction type
    type TXMut: DbTxMut + DbTx;

    /// Create read-only transaction
    fn tx(&self) -> Result<Self::TX, DatabaseError>;

    /// Create read-write transaction
    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError>;

    /// Execute read-only operation atomically
    fn view<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where
        F: FnOnce(&Self::TX) -> T,
    {
        let tx = self.tx()?;
        let res = f(&tx);
        tx.commit()?;
        Ok(res)
    }

    /// Execute read-write operation atomically
    fn update<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where
        F: FnOnce(&Self::TXMut) -> Result<T, DatabaseError>,
    {
        let tx = self.tx_mut()?;
        let res = f(&tx)?;
        tx.commit()?;
        Ok(res)
    }
}
```

### 2. DbTx - Read-Only Transactions

**Location**: `crates/storage/db-api/src/transaction.rs`

```rust
pub trait DbTx: Send + Sync {
    /// Read-only cursor type
    type Cursor<T: Table>: DbCursorRO<T>;

    /// Read-only cursor for DupSort tables
    type DupCursor<T: DupSort>: DbDupCursorRO<T> + DbCursorRO<T>;

    /// Get value by key
    fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>, DatabaseError>;

    /// Create cursor for iteration
    fn cursor_read<T: Table>(&self) -> Result<Self::Cursor<T>, DatabaseError>;

    /// Create cursor for DupSort table
    fn cursor_dup_read<T: DupSort>(&self) -> Result<Self::DupCursor<T>, DatabaseError>;

    /// Get table row count
    fn entries<T: Table>(&self) -> Result<usize, DatabaseError>;

    /// Commit transaction (frees memory)
    fn commit(self) -> Result<bool, DatabaseError>;
}
```

### 3. DbTxMut - Read-Write Transactions

**Location**: `crates/storage/db-api/src/transaction.rs`

```rust
pub trait DbTxMut: Send + Sync {
    /// Read-write cursor type
    type CursorMut<T: Table>: DbCursorRW<T> + DbCursorRO<T>;

    /// Read-write cursor for DupSort tables
    type DupCursorMut<T: DupSort>: DbDupCursorRW<T> + DbDupCursorRO<T>;

    /// Insert or update value
    fn put<T: Table>(&self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Append highest key (O(1) optimization)
    fn append<T: Table>(&self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Delete entry
    fn delete<T: Table>(
        &self,
        key: T::Key,
        value: Option<T::Value>,
    ) -> Result<bool, DatabaseError>;

    /// Clear entire table
    fn clear<T: Table>(&self) -> Result<(), DatabaseError>;

    /// Create write cursor
    fn cursor_write<T: Table>(&self) -> Result<Self::CursorMut<T>, DatabaseError>;
}
```

### 4. Cursor Traits

**Location**: `crates/storage/db-api/src/cursor.rs`

```rust
/// Read-only cursor
pub trait DbCursorRO<T: Table> {
    /// Move to first entry
    fn first(&mut self) -> PairResult<T>;

    /// Move to last entry
    fn last(&mut self) -> PairResult<T>;

    /// Seek to key >= target
    fn seek(&mut self, key: T::Key) -> PairResult<T>;

    /// Seek to exact key
    fn seek_exact(&mut self, key: T::Key) -> PairResult<T>;

    /// Move to next entry
    fn next(&mut self) -> PairResult<T>;

    /// Move to previous entry
    fn prev(&mut self) -> PairResult<T>;

    /// Get current value
    fn current(&mut self) -> PairResult<T>;

    /// Create forward walker
    fn walk(&mut self, start_key: Option<T::Key>) -> Result<Walker<T, Self>, DatabaseError>;

    /// Create backward walker
    fn walk_back(&mut self, start_key: Option<T::Key>) -> Result<ReverseWalker<T, Self>, DatabaseError>;

    /// Create range walker
    fn walk_range(
        &mut self,
        range: impl RangeBounds<T::Key>,
    ) -> Result<RangeWalker<T, Self>, DatabaseError>;
}

/// Read-write cursor
pub trait DbCursorRW<T: Table>: DbCursorRO<T> {
    /// Insert or update at current position
    fn upsert(&mut self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Insert only (error if exists)
    fn insert(&mut self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Append pre-sorted data (O(1))
    fn append(&mut self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Delete current entry
    fn delete_current(&mut self) -> Result<(), DatabaseError>;
}
```

## Working with Tables

### Table Definition

**Location**: `crates/storage/db-api/src/tables/mod.rs`

Tables are defined using the `tables!` macro:

```rust
tables! {
    /// Stores block headers
    table Headers {
        type Key = BlockNumber;
        type Value = Header;
    }

    /// Stores account state (DupSort table)
    table PlainStorageState {
        type Key = Address;          // Main key
        type Value = StorageEntry;   // Value
        type SubKey = B256;          // Subkey for duplicates
    }

    /// Generic table over header type
    table CustomHeaders<H = Header> {
        type Key = BlockNumber;
        type Value = H;
    }
}
```

### Standard Tables

| Table Name | Key | Value | DupSort | Purpose |
|------------|-----|-------|---------|---------|
| `CanonicalHeaders` | BlockNumber | HeaderHash | No | Canonical block hashes |
| `Headers` | BlockNumber | Header | No | Block headers |
| `BlockBodyIndices` | BlockNumber | StoredBlockBodyIndices | No | Transaction indices |
| `Transactions` | TxNumber | TransactionSigned | No | Transaction data |
| `Receipts` | TxNumber | Receipt | No | Transaction receipts |
| `PlainAccountState` | Address | Account | No | Current account state |
| `PlainStorageState` | Address | StorageEntry | Yes (B256) | Current storage |
| `AccountsHistory` | ShardedKey<Address> | BlockNumberList | No | Account change history |
| `StoragesHistory` | StorageShardedKey | BlockNumberList | No | Storage change history |
| `AccountChangeSets` | BlockNumber | AccountBeforeTx | Yes (Address) | Pre-tx account state |
| `StorageChangeSets` | BlockNumberAddress | StorageEntry | Yes (B256) | Pre-tx storage state |
| `Bytecodes` | B256 | Bytecode | No | Contract bytecode |

### Encoding and Compression

**Location**: `crates/storage/db-api/src/table.rs`

Keys and values are automatically encoded/compressed:

```rust
/// Encoding for keys
pub trait Encode {
    type Encoded: AsRef<[u8]>;

    fn encode(self) -> Self::Encoded;
}

/// Decoding for keys
pub trait Decode: Sized {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError>;
}

/// Compression for values
pub trait Compress: Send + Sync + Sized {
    type Compressed: AsRef<[u8]>;

    fn compress(self) -> Self::Compressed;
    fn compress_to_buf<B: bytes::BufMut + AsMut<[u8]>>(self, buf: &mut B);
}

/// Decompression for values
pub trait Decompress: Send + Sync + Sized {
    fn decompress<B: AsRef<[u8]>>(value: B) -> Result<Self, DatabaseError>;
}
```

## MDBX Database

### Configuration

**Location**: `crates/storage/db/src/implementation/mdbx/mod.rs`

```rust
pub struct DatabaseArguments {
    /// Client version
    pub client_version: ClientVersion,

    /// Database size and growth
    pub geometry: Geometry<usize>,

    /// MDBX logging level
    pub log_level: Option<LogLevel>,

    /// Max read transaction duration before warning
    pub max_read_transaction_duration: Option<Duration>,

    /// Exclusive access (for network shares)
    pub exclusive: Option<bool>,

    /// Max concurrent readers
    pub max_readers: Option<u64>,

    /// Sync mode (durable vs fast)
    pub sync_mode: Option<SyncMode>,
}

impl Default for DatabaseArguments {
    fn default() -> Self {
        Self {
            client_version: ClientVersion::default(),
            geometry: Geometry {
                size: Some(0..(8 * TERABYTE)),       // 8TB max
                growth_step: Some(4 * GIGABYTE),     // Grow 4GB at a time
                shrink_threshold: Some(0),
                page_size: Some(default_page_size()),
            },
            max_readers: Some(32_000),
            sync_mode: Some(SyncMode::SafeNoSync),
            // ... other defaults
        }
    }
}
```

### Initialization

**Location**: `crates/storage/db/src/mdbx.rs`

```rust
use reth_db::{init_db, open_db, open_db_read_only, DatabaseArguments};
use reth_db_api::models::ClientVersion;

// Create and initialize new database
let db = init_db(
    "/path/to/db",
    DatabaseArguments::new(ClientVersion::default())
)?;

// Open existing database (read-write)
let db = open_db(
    "/path/to/db",
    DatabaseArguments::new(ClientVersion::default())
)?;

// Open existing database (read-only)
let db = open_db_read_only(
    "/path/to/db",
    DatabaseArguments::new(ClientVersion::default())
)?;
```

### Performance Tuning

```rust
let db = init_db(
    path,
    DatabaseArguments::new(ClientVersion::default())
        // Smaller max size for testing
        .with_geometry_max_size(Some(100 * GIGABYTE))

        // Smaller growth steps
        .with_growth_step(Some(1 * GIGABYTE))

        // Limit concurrent readers
        .with_max_readers(Some(1000))

        // Durable mode for production
        .with_sync_mode(Some(SyncMode::Durable))

        // Exclusive mode for network shares
        .with_exclusive(Some(true))
)?;
```

## Static Files

### What Are Static Files?

Static files store **immutable finalized blockchain history** in compressed columnar archives:

- **Headers**: Block headers, total difficulties, hashes
- **Transactions**: Signed transaction data
- **Receipts**: Transaction execution receipts

### Configuration

**Default**: 500,000 blocks per static file

```rust
pub const DEFAULT_BLOCKS_PER_STATIC_FILE: u64 = 500_000;

// Files created at boundaries:
// - static_file_headers_0_499999
// - static_file_headers_500000_999999
// - static_file_headers_1000000_1499999
// etc.
```

### Segments

**Location**: `crates/static-file/types/src/segment.rs`

```rust
pub enum StaticFileSegment {
    /// Block headers (3 columns: header, total_difficulty, hash)
    Headers,

    /// Transactions (1 column: signed transaction)
    Transactions,

    /// Receipts (1 column: receipt)
    Receipts,
}

impl StaticFileSegment {
    pub const fn columns(&self) -> usize {
        match self {
            Self::Headers => 3,
            Self::Transactions | Self::Receipts => 1,
        }
    }

    pub const fn is_tx_based(&self) -> bool {
        matches!(self, Self::Receipts | Self::Transactions)
    }

    pub const fn is_block_based(&self) -> bool {
        matches!(self, Self::Headers)
    }
}
```

### StaticFileProvider

**Location**: `crates/storage/provider/src/providers/static_file/manager.rs`

```rust
use reth_provider::providers::StaticFileProvider;

// Read-only provider (watches for changes)
let provider = StaticFileProvider::read_only(path, true)?;

// Read-write provider (exclusive access)
let provider = StaticFileProvider::read_write(path)?;

// Get highest block in static files
let highest = provider.get_highest_static_file_block(StaticFileSegment::Headers);

// Get specific segment provider
let jar_provider = provider.get_segment_provider_from_block(
    StaticFileSegment::Headers,
    block_number,
    None,
)?;

// Query data
let header = jar_provider.header_by_number(block_number)??;
```

## Provider Pattern

### ProviderFactory

**Location**: `crates/storage/provider/src/providers/database/mod.rs`

Central factory for creating providers:

```rust
pub struct ProviderFactory<N: NodePrimitives> {
    db: Arc<DatabaseEnv>,
    chain_spec: Arc<ChainSpec>,
    static_file_provider: StaticFileProvider<N>,
}

impl<N: NodePrimitives> ProviderFactory<N> {
    /// Create read-only provider
    pub fn provider(&self) -> ProviderResult<DatabaseProviderRO<DatabaseEnv, N>> {
        Ok(DatabaseProvider::new(
            self.db.tx()?,
            self.chain_spec.clone(),
            self.static_file_provider.clone(),
        ))
    }

    /// Create read-write provider
    pub fn provider_rw(&self) -> ProviderResult<DatabaseProviderRW<DatabaseEnv, N>> {
        Ok(DatabaseProviderRW(DatabaseProvider::new_rw(
            self.db.tx_mut()?,
            self.chain_spec.clone(),
            self.static_file_provider.clone(),
        )))
    }

    /// Get static file provider
    pub fn static_file_provider(&self) -> StaticFileProvider<N> {
        self.static_file_provider.clone()
    }
}
```

### DatabaseProvider

**Location**: `crates/storage/provider/src/providers/database/provider.rs`

Provides unified access to MDBX and static files:

```rust
pub struct DatabaseProvider<TX, N: NodePrimitives> {
    tx: TX,
    chain_spec: Arc<N::ChainSpec>,
    static_file_provider: StaticFileProvider<N>,
    prune_modes: PruneModes,
}

// Implements many provider traits:
impl<TX, N> BlockHashReader for DatabaseProvider<TX, N> { ... }
impl<TX, N> BlockNumReader for DatabaseProvider<TX, N> { ... }
impl<TX, N> BlockReader for DatabaseProvider<TX, N> { ... }
impl<TX, N> TransactionsProvider for DatabaseProvider<TX, N> { ... }
impl<TX, N> ReceiptProvider for DatabaseProvider<TX, N> { ... }
impl<TX, N> AccountReader for DatabaseProvider<TX, N> { ... }
```

### State Providers

#### Latest State

**Location**: `crates/storage/provider/src/providers/state/latest.rs`

```rust
pub struct LatestStateProviderRef<'b, Provider> {
    provider: &'b Provider,
}

impl<Provider> AccountReader for LatestStateProviderRef<'_, Provider>
where
    Provider: DBProvider + BlockHashReader,
{
    fn basic_account(&self, address: Address) -> ProviderResult<Option<Account>> {
        self.provider.tx_ref().get::<tables::PlainAccountState>(address)
    }
}
```

#### Historical State

**Location**: `crates/storage/provider/src/providers/state/historical.rs`

```rust
pub struct HistoricalStateProviderRef<'b, Provider> {
    provider: &'b Provider,
    block_number: BlockNumber,
    state_provider: Arc<StateProviderBox>,
}

impl<Provider> AccountReader for HistoricalStateProviderRef<'_, Provider> {
    fn basic_account(&self, address: Address) -> ProviderResult<Option<Account>> {
        // Look up historical account state using history indices
        // and changesets to find the state at target block
    }
}
```

## Reading and Writing Data

### Simple Reads

```rust
// Using transaction directly
let tx = db.tx()?;
let header = tx.get::<Headers>(block_number)?;
tx.commit()?;

// Using view helper
let header = db.view(|tx| {
    tx.get::<Headers>(block_number)
})?;

// Using provider
let provider = factory.provider()?;
let header = provider.header_by_number(block_number)?;
```

### Cursor Iteration

**Forward iteration**:

```rust
let tx = db.tx()?;
let mut cursor = tx.cursor_read::<Headers>()?;

// Walk from start
for result in cursor.walk(Some(start_block))? {
    let (block_num, header) = result?;
    println!("Block {}: {}", block_num, header.hash());
}
```

**Reverse iteration**:

```rust
let mut cursor = tx.cursor_read::<Headers>()?;

for result in cursor.walk_back(Some(end_block))? {
    let (block_num, header) = result?;
    // Process in reverse
}
```

**Range iteration**:

```rust
let mut cursor = tx.cursor_read::<Headers>()?;

for result in cursor.walk_range(1000..=2000)? {
    let (block_num, header) = result?;
    // Only blocks 1000-2000
}
```

### DupSort Tables

```rust
let tx = db.tx()?;
let mut cursor = tx.cursor_dup_read::<PlainStorageState>()?;

// Walk all storage slots for an address
for result in cursor.walk_dup(Some(address), None)? {
    let (addr, slot, value) = result?;
    println!("Slot {}: {:?}", slot, value);
}
```

### Simple Writes

```rust
// Using transaction
let tx = db.tx_mut()?;
tx.put::<Headers>(block_number, header)?;
tx.commit()?;

// Using update helper
db.update(|tx| {
    tx.put::<Headers>(block_number, header)?;
    tx.put::<CanonicalHeaders>(block_number, hash)?;
    Ok(())
})?;
```

### Batch Writes

```rust
db.update(|tx| {
    let mut cursor = tx.cursor_write::<Headers>()?;

    for header in headers {
        // Append if keys are pre-sorted (O(1))
        cursor.append(header.number, &header)?;
    }

    Ok(())
})?;
```

### Delete Operations

```rust
// Delete single entry
tx.delete::<PlainAccountState>(address, None)?;

// Clear entire table
tx.clear::<PlainAccountState>()?;

// Delete via cursor
let mut cursor = tx.cursor_write::<PlainAccountState>()?;
cursor.seek_exact(address)?;
cursor.delete_current()?;
```

### Static File Writes

```rust
let provider = factory.static_file_provider();
let mut writer = provider.latest_writer(StaticFileSegment::Headers)?;

// Append headers
for header in headers {
    writer.append_header(&header, &header.hash())?;
}

// Commit to disk
writer.commit()?;
```

## Custom Tables

### Defining Custom Tables

Add to `tables!` macro in `crates/storage/db-api/src/tables/mod.rs`:

```rust
tables! {
    // ... existing tables ...

    /// My custom table for tracking validators
    table ValidatorRegistry {
        type Key = u64;           // Validator index
        type Value = Address;      // Validator address
    }

    /// DupSort table for validator history
    table ValidatorHistory {
        type Key = u64;           // Validator index
        type Value = EpochData;    // Data per epoch
        type SubKey = u64;         // Epoch number
    }
}
```

### Custom Encoding

If your key/value types don't implement `Encode`/`Decode`:

```rust
#[derive(Debug, Clone)]
pub struct MyCustomKey {
    pub field1: u64,
    pub field2: B256,
}

impl Encode for MyCustomKey {
    type Encoded = Vec<u8>;

    fn encode(self) -> Self::Encoded {
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(&self.field1.to_be_bytes());
        buf.extend_from_slice(self.field2.as_slice());
        buf
    }
}

impl Decode for MyCustomKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        if value.len() != 40 {
            return Err(DatabaseError::Decode);
        }

        let field1 = u64::from_be_bytes(value[0..8].try_into().unwrap());
        let field2 = B256::from_slice(&value[8..40]);

        Ok(Self { field1, field2 })
    }
}
```

### Usage Example

```rust
// Write to custom table
db.update(|tx| {
    tx.put::<ValidatorRegistry>(validator_index, validator_address)?;
    Ok(())
})?;

// Read from custom table
let address = db.view(|tx| {
    tx.get::<ValidatorRegistry>(validator_index)
})?;

// Iterate custom DupSort table
db.view(|tx| {
    let mut cursor = tx.cursor_dup_read::<ValidatorHistory>()?;

    for result in cursor.walk_dup(Some(validator_index), None)? {
        let (index, epoch, data) = result?;
        // Process validator data per epoch
    }

    Ok::<_, DatabaseError>(())
})?;
```

## Best Practices

### 1. Keep Transactions Short

```rust
// ❌ BAD: Long-lived transaction
let tx = db.tx()?;
for item in huge_iterator {
    expensive_computation(tx, item);  // Blocks other writers!
}
tx.commit()?;

// ✅ GOOD: Chunked transactions
for chunk in huge_iterator.chunks(1000) {
    db.update(|tx| {
        for item in chunk {
            tx.put::<MyTable>(item.key, item.value)?;
        }
        Ok(())
    })?;
}
```

### 2. Use Cursors for Iteration

```rust
// ❌ BAD: Random access in loop
for i in 0..1000 {
    let header = tx.get::<Headers>(i)?;
}

// ✅ GOOD: Cursor iteration
let mut cursor = tx.cursor_read::<Headers>()?;
for result in cursor.walk(Some(0))?.take(1000) {
    let (num, header) = result?;
}
```

### 3. Use Append for Sequential Writes

```rust
// ✅ OPTIMAL: Use append for sorted keys (O(1) instead of O(logN))
let mut cursor = tx.cursor_write::<Headers>()?;
for header in sorted_headers {
    cursor.append(header.number, &header)?;
}
```

### 4. Batch Related Operations

```rust
// ✅ GOOD: Single transaction for related writes
db.update(|tx| {
    tx.put::<Headers>(block_num, header)?;
    tx.put::<CanonicalHeaders>(block_num, hash)?;
    tx.put::<BlockBodyIndices>(block_num, indices)?;
    Ok(())
})?;
```

### 5. Use Providers for Business Logic

```rust
// ❌ BAD: Direct database access in application code
let tx = db.tx()?;
let account = tx.get::<PlainAccountState>(address)?;

// ✅ GOOD: Use providers
let provider = factory.provider()?;
let account = provider.basic_account(address)?;
```

## Key Takeaways

1. **Hybrid Storage**: MDBX for hot data, static files for cold data
2. **Trait Abstraction**: Database traits allow different implementations
3. **Provider Pattern**: Unified access to both MDBX and static files
4. **Cursors for Iteration**: More efficient than repeated key lookups
5. **Short Transactions**: Keep transactions brief to avoid blocking
6. **Type Safety**: Tables are type-safe with compile-time checks
7. **Compression**: Automatic encoding/compression for keys/values
8. **Static Files**: Efficient for immutable historical data

## Next Steps

- Read the [Node Builder Guide](./NODE_BUILDER_GUIDE.md) for provider integration
- Study `crates/storage/db-api/src/tables/mod.rs` for all available tables
- Explore `crates/storage/provider/` for provider implementations
- Check `crates/storage/db/src/mdbx.rs` for MDBX configuration options
- Review static file examples in `crates/static-file/`
