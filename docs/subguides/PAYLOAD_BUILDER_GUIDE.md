# Payload Builder Guide

This guide explains how to implement custom payload builders (block builders) in Reth, including transaction selection, ordering, and integration with the Engine API.

## Table of Contents

1. [Overview](#overview)
2. [Core Traits](#core-traits)
3. [Transaction Selection](#transaction-selection)
4. [Payload Attributes](#payload-attributes)
5. [Complete Examples](#complete-examples)
6. [Advanced Patterns](#advanced-patterns)

## Overview

The payload builder is responsible for constructing new blocks when requested by the consensus layer via `forkchoiceUpdated`. It continuously tries to build better blocks (higher fees) until the CL requests the payload via `getPayload`.

### Key Components

- **PayloadBuilder**: Trait for building blocks from attributes
- **PayloadJob**: Continuously running job that builds progressively better payloads
- **PayloadJobGenerator**: Creates new payload jobs from attributes
- **PayloadBuilderService**: Manages multiple concurrent payload jobs
- **BuildArguments**: Contains state and configuration for building

### Payload Building Flow

```
forkchoiceUpdated (with attributes)
         ↓
PayloadJobGenerator::new_payload_job()
         ↓
PayloadJob spawned
         ├─ Continuously calls PayloadBuilder::try_build()
         ├─ Compares new payload with best so far
         └─ Updates best_payload if better
         ↓
getPayload (1 second deadline)
         ↓
PayloadJob::resolve()
         ↓
Returns best payload built so far
```

## Core Traits

### 1. PayloadBuilder - Building Logic

**Location**: `crates/payload/basic/src/lib.rs:808-854`

Core trait for block building:

```rust
pub trait PayloadBuilder: Send + Sync + Clone {
    /// Payload attributes type
    type Attributes: PayloadBuilderAttributes;

    /// Built payload type
    type BuiltPayload: BuiltPayload;

    /// Try to build a payload
    ///
    /// Called repeatedly by PayloadJob. Returns:
    /// - Better: New payload is better than current best
    /// - Aborted: New payload is not better, keep current best
    /// - Cancelled: Job was cancelled
    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError>;

    /// Handle missing payload (called before first build completes)
    ///
    /// Options:
    /// - RaceEmptyPayload: Build empty payload quickly
    /// - AwaitInProgress: Wait for first build to complete
    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::RaceEmptyPayload
    }

    /// Build empty payload (fallback when no transactions)
    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError>;
}
```

### BuildArguments

Contains everything needed for building:

```rust
pub struct BuildArguments<Attributes, Payload> {
    /// Client for state access
    pub client: Arc<dyn StateProviderFactory>,

    /// Transaction pool
    pub pool: Arc<dyn TransactionPool>,

    /// Cached reads from previous builds
    pub cached_reads: CachedReads,

    /// Payload configuration
    pub config: PayloadConfig<Attributes, Payload>,

    /// Cancellation signal
    pub cancel: CancellationToken,

    /// Current best payload
    pub best_payload: Option<Payload>,
}

pub struct PayloadConfig<Attributes, ParentHeader> {
    /// Payload attributes from CL
    pub attributes: Attributes,

    /// Parent block header
    pub parent_header: ParentHeader,

    /// Extra data for block
    pub extra_data: Bytes,
}
```

### BuildOutcome

Result of build attempt:

```rust
pub enum BuildOutcome<Payload> {
    /// New payload is better (higher fees)
    Better { payload: Payload },

    /// Build aborted (new payload not better)
    Aborted { fees: U256, cached_reads: CachedReads },

    /// Build was cancelled
    Cancelled,

    /// Freeze job (stop building)
    Freeze(Payload),
}
```

### 2. PayloadJob - Continuous Building

**Location**: `crates/payload/builder/src/traits.rs:20-82`

Future that continuously builds better payloads:

```rust
pub trait PayloadJob: Future<Output = Result<(), PayloadBuilderError>> {
    /// Payload attributes type
    type PayloadAttributes: PayloadBuilderAttributes + Debug;

    /// Future for resolving payload
    type ResolvePayloadFuture: Future<Output = Result<Self::BuiltPayload, PayloadBuilderError>>
        + Send
        + 'static;

    /// Built payload type
    type BuiltPayload: BuiltPayload + Clone + Debug;

    /// Get best payload built so far
    fn best_payload(&self) -> Result<Self::BuiltPayload, PayloadBuilderError>;

    /// Get payload attributes
    fn payload_attributes(&self) -> Result<Self::PayloadAttributes, PayloadBuilderError>;

    /// Get payload timestamp
    fn payload_timestamp(&self) -> Result<u64, PayloadBuilderError> {
        Ok(self.payload_attributes()?.timestamp())
    }

    /// Resolve payload for given kind
    ///
    /// Called when CL requests payload via getPayload.
    /// Must resolve within 1 second.
    ///
    /// Returns:
    /// - Future that resolves to payload
    /// - Whether to keep job alive after resolution
    fn resolve_kind(
        &mut self,
        kind: PayloadKind,
    ) -> (Self::ResolvePayloadFuture, KeepPayloadJobAlive);

    /// Resolve payload (any kind)
    fn resolve(&mut self) -> (Self::ResolvePayloadFuture, KeepPayloadJobAlive) {
        self.resolve_kind(PayloadKind::WaitForPending)
    }
}
```

### PayloadKind

How to resolve payload:

```rust
pub enum PayloadKind {
    /// Wait for pending build to complete
    WaitForPending,

    /// Return best so far, or empty if nothing built
    Earliest,
}
```

### 3. PayloadJobGenerator - Creating Jobs

**Location**: `crates/payload/builder/src/traits.rs:94-121`

Creates new payload jobs:

```rust
pub trait PayloadJobGenerator {
    type Job: PayloadJob;

    /// Create new payload job from attributes
    ///
    /// Called when forkchoiceUpdated includes payload attributes.
    fn new_payload_job(
        &self,
        attr: <Self::Job as PayloadJob>::PayloadAttributes,
    ) -> Result<Self::Job, PayloadBuilderError>;

    /// Handle chain state changes
    ///
    /// Called when canonical chain updates. Can be used to
    /// invalidate cached state or adjust pending builds.
    fn on_new_state<N: NodePrimitives>(
        &mut self,
        new_state: CanonStateNotification<N>,
    ) {
        let _ = new_state;
    }
}
```

### 4. BuiltPayload - Result Type

**Location**: `crates/payload/primitives/src/traits.rs`

Trait for successfully built payload:

```rust
pub trait BuiltPayload: Send + Sync + Clone + Unpin + Debug {
    type Primitives: NodePrimitives;

    /// Unique payload ID
    fn id(&self) -> PayloadId;

    /// The built block
    fn block(&self) -> &SealedBlock<<Self::Primitives as NodePrimitives>::Block>;

    /// Total fees collected
    fn fees(&self) -> U256;

    /// Blob sidecars (Cancun+)
    fn blob_sidecars(&self) -> Option<&BlobSidecars> {
        None
    }

    /// Execution requests (Prague+)
    fn execution_requests(&self) -> Option<&Requests> {
        None
    }

    /// Executed block with receipts
    fn executed_block(&self) -> Option<ExecutedBlock<...>> {
        None
    }
}
```

## Transaction Selection

### PayloadTransactions Trait

**Location**: `crates/payload/util/src/traits.rs:1-45`

Abstraction for transaction iteration:

```rust
pub trait PayloadTransactions {
    type Transaction;

    /// Get next transaction
    fn next(&mut self, ctx: ()) -> Option<Self::Transaction>;

    /// Mark transaction as invalid
    ///
    /// Called when transaction fails validation or execution.
    /// Should skip future transactions from same sender with same/higher nonce.
    fn mark_invalid(&mut self, sender: Address, nonce: u64);
}
```

### Built-in Implementations

**1. BestPayloadTransactions** - Wraps mempool:

```rust
impl<Pool> PayloadTransactions for BestPayloadTransactions<Pool>
where
    Pool: TransactionPool,
{
    type Transaction = Arc<ValidPoolTransaction<Pool::Transaction>>;

    fn next(&mut self, _: ()) -> Option<Self::Transaction> {
        self.best.next()
    }

    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.best.mark_invalid(sender, nonce)
    }
}
```

**2. PayloadTransactionsChain** - Chains multiple sources:

```rust
pub struct PayloadTransactionsChain<T> {
    sources: Vec<(T, u64)>,  // (source, max_count)
    current_index: usize,
    current_count: u64,
}

impl<T: PayloadTransactions> PayloadTransactions for PayloadTransactionsChain<T> {
    type Transaction = T::Transaction;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        loop {
            let (source, limit) = self.sources.get_mut(self.current_index)?;

            if self.current_count >= *limit {
                // Move to next source
                self.current_index += 1;
                self.current_count = 0;
                continue;
            }

            if let Some(tx) = source.next(ctx) {
                self.current_count += 1;
                return Some(tx);
            }

            // Current source exhausted, move to next
            self.current_index += 1;
            self.current_count = 0;
        }
    }
}
```

### Custom Transaction Ordering Example

```rust
pub struct PriorityTransactions<Pool> {
    pool: Pool,
    priority_addresses: HashSet<Address>,
    priority_txs: VecDeque<Arc<ValidPoolTransaction<Pool::Transaction>>>,
    regular_txs: BestPayloadTransactions<Pool>,
}

impl<Pool: TransactionPool> PayloadTransactions for PriorityTransactions<Pool> {
    type Transaction = Arc<ValidPoolTransaction<Pool::Transaction>>;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        // First, drain priority transactions
        if let Some(tx) = self.priority_txs.pop_front() {
            return Some(tx);
        }

        // Then get from regular pool, filtering priority addresses
        loop {
            let tx = self.regular_txs.next(ctx)?;

            // Skip if from priority address (already processed)
            if self.priority_addresses.contains(&tx.sender()) {
                continue;
            }

            return Some(tx);
        }
    }

    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.regular_txs.mark_invalid(sender, nonce);
        self.priority_txs.retain(|tx| {
            tx.sender() != sender || tx.nonce() < nonce
        });
    }
}
```

## Payload Attributes

### PayloadBuilderAttributes Trait

**Location**: `crates/payload/primitives/src/traits.rs:44-88`

Extended attributes used during building:

```rust
pub trait PayloadBuilderAttributes: Send + Sync + Unpin + fmt::Debug + 'static {
    /// RPC attributes type (from CL)
    type RpcPayloadAttributes: Send + Sync + 'static;

    /// Error type
    type Error: core::error::Error + Send + Sync + 'static;

    /// Create from RPC attributes
    fn try_new(
        parent: B256,
        rpc_payload_attributes: Self::RpcPayloadAttributes,
        version: u8,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized;

    /// Unique payload ID
    fn payload_id(&self) -> PayloadId;

    /// Parent block hash
    fn parent(&self) -> B256;

    /// Target timestamp
    fn timestamp(&self) -> u64;

    /// Parent beacon block root (Cancun+)
    fn parent_beacon_block_root(&self) -> Option<B256>;

    /// Fee recipient
    fn suggested_fee_recipient(&self) -> Address;

    /// Random value (prev_randao)
    fn prev_randao(&self) -> B256;

    /// Withdrawals (Shanghai+)
    fn withdrawals(&self) -> &Withdrawals;
}
```

### Ethereum Payload Attributes

**Location**: `crates/ethereum/engine-primitives/src/payload.rs:315-404`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EthPayloadBuilderAttributes {
    /// Payload ID
    pub id: PayloadId,
    /// Parent block hash
    pub parent: B256,
    /// Target timestamp
    pub timestamp: u64,
    /// Fee recipient
    pub suggested_fee_recipient: Address,
    /// Random value
    pub prev_randao: B256,
    /// Withdrawals
    pub withdrawals: Withdrawals,
    /// Parent beacon block root
    pub parent_beacon_block_root: Option<B256>,
}

impl PayloadBuilderAttributes for EthPayloadBuilderAttributes {
    type RpcPayloadAttributes = PayloadAttributes;
    type Error = Infallible;

    fn try_new(
        parent: B256,
        rpc_attributes: Self::RpcPayloadAttributes,
        _version: u8,
    ) -> Result<Self, Self::Error> {
        let id = payload_id(&parent, &rpc_attributes);

        Ok(Self {
            id,
            parent,
            timestamp: rpc_attributes.timestamp,
            suggested_fee_recipient: rpc_attributes.suggested_fee_recipient,
            prev_randao: rpc_attributes.prev_randao,
            withdrawals: rpc_attributes.withdrawals.unwrap_or_default().into(),
            parent_beacon_block_root: rpc_attributes.parent_beacon_block_root,
        })
    }

    // ... implement accessors
}
```

### Payload ID Generation

Deterministic hash of attributes:

```rust
pub fn payload_id(parent: &B256, attributes: &PayloadAttributes) -> PayloadId {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(parent.as_slice());
    hasher.update(&attributes.timestamp.to_be_bytes());
    hasher.update(attributes.prev_randao.as_slice());
    hasher.update(attributes.suggested_fee_recipient.as_slice());

    if let Some(withdrawals) = &attributes.withdrawals {
        let mut buf = Vec::new();
        withdrawals.encode(&mut buf);
        hasher.update(buf);
    }

    if let Some(root) = attributes.parent_beacon_block_root {
        hasher.update(root.as_slice());
    }

    let out = hasher.finalize();
    PayloadId::new(out.as_slice()[..8].try_into().expect("sufficient length"))
}
```

## Complete Examples

### Example 1: Ethereum Payload Builder

**Location**: `crates/ethereum/payload/src/lib.rs:78-374`

```rust
#[derive(Debug, Clone)]
pub struct EthereumPayloadBuilder<Pool, Client, EvmConfig = EthEvmConfig> {
    client: Client,
    pool: Pool,
    evm_config: EvmConfig,
    builder_config: EthereumBuilderConfig,
}

impl<Pool, Client, EvmConfig> PayloadBuilder for EthereumPayloadBuilder<Pool, Client, EvmConfig>
where
    EvmConfig: ConfigureEvm<Primitives = EthPrimitives>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthereumHardforks>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        default_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            args,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, Header>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        default_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            BuildArguments { /* empty */ },
            |_| NoopPayloadTransactions::default(),
        )?
        .into_payload()
        .ok_or(PayloadBuilderError::MissingPayload)
    }
}
```

### The Building Function

```rust
pub fn default_ethereum_payload<EvmConfig, Client, Pool, F>(
    evm_config: EvmConfig,
    client: Client,
    pool: Pool,
    builder_config: EthereumBuilderConfig,
    args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
    best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    F: FnOnce(BestTransactionsAttributes) -> BestTransactionsIter<Pool>,
{
    let BuildArguments { client, pool, cached_reads, config, cancel, best_payload } = args;

    // 1. Create state from parent block
    let state_provider = client.state_by_block_hash(config.parent_header.hash())?;
    let mut db = CachedReadsDbMut::new(state_provider, cached_reads);

    // 2. Configure EVM for next block
    let mut evm = evm_config.evm_with_env_for_next_block(
        &mut db,
        &config.parent_header,
        &config.attributes,
    )?;

    // 3. Get best transactions from pool
    let mut txs = best_txs(BestTransactionsAttributes::new(
        config.attributes.timestamp,
        config.attributes.suggested_fee_recipient,
    ));

    // 4. Build block by executing transactions
    let mut cumulative_gas_used = 0u64;
    let mut sum_blob_gas_used = 0u64;
    let mut executed_txs = Vec::new();
    let mut receipts = Vec::new();
    let mut total_fees = U256::ZERO;

    let block_gas_limit = builder_config.gas_limit(config.parent_header.gas_limit);
    let base_fee = config.attributes.base_fee_per_gas;

    while let Some(tx) = txs.next() {
        // Check cancellation
        if cancel.is_cancelled() {
            return Ok(BuildOutcome::Cancelled);
        }

        // Check gas limit
        if cumulative_gas_used + tx.gas_limit() > block_gas_limit {
            txs.mark_invalid(tx.sender(), tx.nonce());
            continue;
        }

        // Validate blob transaction (Cancun+)
        if let Some(blob_tx) = tx.transaction.as_eip4844() {
            if sum_blob_gas_used + blob_tx.blob_gas() > MAX_DATA_GAS_PER_BLOCK {
                txs.mark_invalid(tx.sender(), tx.nonce());
                continue;
            }
        }

        // Execute transaction
        let result = evm.transact(tx.clone())?;

        let gas_used = result.gas_used();
        cumulative_gas_used += gas_used;

        // Collect fees
        let priority_fee = tx.effective_tip_per_gas(base_fee).unwrap_or_default();
        total_fees += U256::from(gas_used) * U256::from(priority_fee);

        // Build receipt
        let receipt = Receipt {
            tx_type: tx.tx_type(),
            success: result.is_success(),
            cumulative_gas_used,
            logs: result.into_logs(),
        };

        executed_txs.push(tx.into_transaction());
        receipts.push(receipt);

        if let Some(blob_tx) = tx.transaction.as_eip4844() {
            sum_blob_gas_used += blob_tx.blob_gas();
        }
    }

    // 5. Compare with best_payload
    let better = best_payload
        .as_ref()
        .map_or(true, |best| total_fees > best.fees());

    if !better {
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads: db.take_reads(),
        });
    }

    // 6. Assemble final block
    let block = build_block(
        &config,
        executed_txs,
        receipts,
        cumulative_gas_used,
        sum_blob_gas_used,
        &mut db,
    )?;

    let built_payload = EthBuiltPayload::new(
        config.attributes.id,
        Arc::new(block.seal_slow()),
        total_fees,
        None, // requests (Prague+)
    );

    Ok(BuildOutcome::Better { payload: built_payload })
}
```

### Example 2: Optimism Payload Builder

**Location**: `crates/optimism/payload/src/builder.rs:255-305`

Key differences:

```rust
#[derive(Debug)]
pub struct OpPayloadBuilder<Pool, Client, Evm, Txs = ()> {
    pub compute_pending_block: bool,
    pub evm_config: Evm,
    pub pool: Pool,
    pub client: Client,
    pub config: OpBuilderConfig,  // DA constraints
    pub bridge_intercept: BridgeInterceptConfig,
    pub best_transactions: Txs,
}

impl<Pool, Client, Evm> PayloadBuilder for OpPayloadBuilder<Pool, Client, Evm>
where
    // ... bounds
{
    type Attributes = OpPayloadBuilderAttributes;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(&self, args: BuildArguments<...>) -> Result<BuildOutcome<...>, ...> {
        let mut txs = if args.config.attributes.no_tx_pool {
            // Sequencer mode: use only provided transactions
            NoopPayloadTransactions::default()
        } else {
            // Validator mode: use mempool
            self.best_transactions(...)
        };

        // Chain sequencer txs before mempool
        let chained_txs = PayloadTransactionsChain::new([
            (sequencer_txs, u64::MAX),  // All sequencer txs
            (txs, u64::MAX),             // Then mempool
        ]);

        // Build with DA constraints
        build_with_da_limits(chained_txs, &self.config.da_config, ...)
    }
}
```

### Example 3: MEV Payload Builder

```rust
pub struct MevPayloadBuilder<Pool, Client, Evm> {
    mev_relay_url: String,
    fallback: EthereumPayloadBuilder<Pool, Client, Evm>,
}

impl<Pool, Client, Evm> PayloadBuilder for MevPayloadBuilder<Pool, Client, Evm>
where
    // ... same bounds as EthereumPayloadBuilder
{
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        // Try to fetch from MEV relay
        if let Ok(mev_payload) = self.fetch_from_relay(&args.config.attributes) {
            // Validate MEV payload
            if self.validate_mev_payload(&mev_payload, &args.config) {
                return Ok(BuildOutcome::Better { payload: mev_payload });
            }
        }

        // Fall back to local building
        self.fallback.try_build(args)
    }

    fn build_empty_payload(&self, config: PayloadConfig<...>) -> Result<...> {
        self.fallback.build_empty_payload(config)
    }
}
```

## Advanced Patterns

### Pattern 1: Multi-Source Transaction Ordering

```rust
pub fn build_with_priority<Pool>(
    pool: Pool,
    priority_addresses: HashSet<Address>,
) -> impl PayloadTransactions
where
    Pool: TransactionPool,
{
    // Split transactions by priority
    let (priority_txs, regular_txs) = split_by_priority(pool, &priority_addresses);

    // Chain: priority first, then regular
    PayloadTransactionsChain::new([
        (priority_txs, u64::MAX),
        (regular_txs, u64::MAX),
    ])
}
```

### Pattern 2: Data Availability Constraints

```rust
pub fn build_with_da_limit<Txs>(
    txs: Txs,
    max_da_size: u64,
) -> impl PayloadTransactions
where
    Txs: PayloadTransactions,
{
    DAConstrainedTransactions {
        inner: txs,
        current_da_size: 0,
        max_da_size,
    }
}

struct DAConstrainedTransactions<T> {
    inner: T,
    current_da_size: u64,
    max_da_size: u64,
}

impl<T: PayloadTransactions> PayloadTransactions for DAConstrainedTransactions<T> {
    type Transaction = T::Transaction;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        loop {
            let tx = self.inner.next(ctx)?;

            // Calculate DA size (e.g., compressed tx size for rollups)
            let da_size = calculate_da_size(&tx);

            if self.current_da_size + da_size > self.max_da_size {
                // Skip transaction, exceeds DA limit
                continue;
            }

            self.current_da_size += da_size;
            return Some(tx);
        }
    }
}
```

### Pattern 3: Conditional Building

```rust
impl PayloadBuilder for ConditionalPayloadBuilder {
    fn try_build(&self, args: BuildArguments<...>) -> Result<BuildOutcome<...>> {
        let fees_threshold = self.calculate_threshold(&args.best_payload);

        // Build payload
        let outcome = self.inner.try_build(args)?;

        match outcome {
            BuildOutcome::Better { payload } => {
                // Only return if fees exceed threshold
                if payload.fees() > fees_threshold {
                    Ok(BuildOutcome::Better { payload })
                } else {
                    Ok(BuildOutcome::Aborted {
                        fees: payload.fees(),
                        cached_reads: CachedReads::default(),
                    })
                }
            }
            other => Ok(other),
        }
    }
}
```

## Key Takeaways

1. **Continuous Improvement**: PayloadJob continuously builds better payloads
2. **Transaction Flexibility**: Chain multiple sources with different priorities
3. **Constraint Handling**: Gas limits, DA limits, custom rules
4. **Caching**: Reuse state reads between builds for performance
5. **Cancellation**: Respect cancel signals for timely responses

## Next Steps

- Read the [Engine & Consensus Guide](./ENGINE_CONSENSUS_GUIDE.md) for payload validation
- Read the [Chain Spec & EVM Guide](./CHAINSPEC_EVM_GUIDE.md) for EVM configuration
- Study the Optimism payload builder for L2 patterns
- Experiment with custom transaction ordering strategies
