# Engine API & Consensus Guide

This guide explains Reth's Engine API architecture and consensus validation system, essential for understanding how blocks are validated and integrated into the chain.

## Table of Contents

1. [Overview](#overview)
2. [Key Traits](#key-traits)
3. [Engine API Messages](#engine-api-messages)
4. [Block Validation Pipeline](#block-validation-pipeline)
5. [Custom Implementations](#custom-implementations)
6. [Integration Points](#integration-points)

## Overview

The Engine API is how the consensus layer (CL) communicates with the execution layer (EL). Reth implements this through a sophisticated trait-based system that allows for customization at every level.

### Key Components

- **EngineTypes**: Defines payload envelope versions for different hardforks
- **PayloadTypes**: Core abstraction for execution data and payloads
- **PayloadValidator**: Validates payloads before and after execution
- **Consensus**: Block validation according to consensus rules
- **EngineApiTreeHandler**: Processes Engine API messages and manages block tree

### Engine API Flow

```
Consensus Layer (Beacon Chain)
         ↓
    Engine API
         ↓
┌────────────────────────┐
│  newPayload            │ ← Block from CL
│  forkchoiceUpdated     │ ← Fork choice + build request
│  getPayload            │ ← Request built block
└────────────────────────┘
         ↓
┌────────────────────────┐
│ EngineApiTreeHandler   │
│  - Validate payload    │
│  - Execute block       │
│  - Update fork choice  │
│  - Build new blocks    │
└────────────────────────┘
```

## Key Traits

### 1. EngineTypes - Root Trait

**Location**: `crates/engine/primitives/src/lib.rs:58-108`

Defines versioned payload envelopes for Engine API:

```rust
pub trait EngineTypes:
    PayloadTypes<
        BuiltPayload: TryInto<Self::ExecutionPayloadEnvelopeV1>
                    + TryInto<Self::ExecutionPayloadEnvelopeV2>
                    + TryInto<Self::ExecutionPayloadEnvelopeV3>
                    + TryInto<Self::ExecutionPayloadEnvelopeV4>
                    + TryInto<Self::ExecutionPayloadEnvelopeV5>,
    > + DeserializeOwned
    + Serialize
{
    /// Pre-Paris payload format (V1)
    type ExecutionPayloadEnvelopeV1: /* serializable, etc. */;

    /// Shanghai payload with withdrawals (V2)
    type ExecutionPayloadEnvelopeV2: /* serializable, etc. */;

    /// Cancun payload with blobs (V3)
    type ExecutionPayloadEnvelopeV3: /* serializable, etc. */;

    /// Prague payload with execution requests (V4)
    type ExecutionPayloadEnvelopeV4: /* serializable, etc. */;

    /// Osaka payload (V5)
    type ExecutionPayloadEnvelopeV5: /* serializable, etc. */;
}
```

**Example - Ethereum**:

```rust
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct EthEngineTypes<T: PayloadTypes = EthPayloadTypes> {
    _marker: core::marker::PhantomData<T>,
}

impl<T: PayloadTypes<ExecutionData = ExecutionData>> EngineTypes for EthEngineTypes<T> {
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = ExecutionPayloadEnvelopeV3;
    type ExecutionPayloadEnvelopeV4 = ExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = ExecutionPayloadEnvelopeV5;
}
```

### 2. PayloadTypes - Core Payload Abstraction

**Location**: `crates/payload/primitives/src/lib.rs:37-67`

Fundamental trait for payload handling:

```rust
pub trait PayloadTypes: Send + Sync + Unpin + Debug + Clone + 'static {
    /// Execution data format (from consensus layer)
    type ExecutionData: ExecutionPayload;

    /// Successfully built payload
    type BuiltPayload: BuiltPayload + Clone + Unpin;

    /// Attributes from consensus layer
    type PayloadAttributes: PayloadAttributes + Unpin;

    /// Extended attributes for building
    type PayloadBuilderAttributes: PayloadBuilderAttributes + Clone + Unpin;

    /// Convert sealed block to execution payload format
    fn block_to_payload(block: SealedBlock<...>) -> Self::ExecutionData;
}
```

**Example - Ethereum**:

```rust
#[derive(Debug, Default, Clone)]
pub struct EthPayloadTypes;

impl PayloadTypes for EthPayloadTypes {
    type ExecutionData = ExecutionData;
    type BuiltPayload = EthBuiltPayload;
    type PayloadAttributes = EthPayloadAttributes;
    type PayloadBuilderAttributes = EthPayloadBuilderAttributes;

    fn block_to_payload(block: SealedBlock<...>) -> Self::ExecutionData {
        let (payload, sidecar) = ExecutionPayload::from_block_unchecked(
            block.hash(),
            &block.into_block()
        );
        ExecutionData { payload, sidecar }
    }
}
```

### 3. PayloadValidator - Pre/Post Execution Validation

**Location**: `crates/engine/primitives/src/lib.rs:128-176`

Validates payloads before and after execution:

```rust
pub trait PayloadValidator<Types: PayloadTypes>: Send + Sync + Unpin + 'static {
    type Block: Block;

    /// Validate block structure before execution
    ///
    /// Checks:
    /// - Block hash matches
    /// - Base fee is present (if required)
    /// - Extra data size
    /// - Transactions are well-formed
    /// - Blob transactions (Cancun+)
    /// - Execution requests (Prague+)
    fn ensure_well_formed_payload(
        &self,
        payload: Types::ExecutionData,
    ) -> Result<RecoveredBlock<Self::Block>, NewPayloadError>;

    /// Verify post-execution state transitions
    ///
    /// Called after executing the block to verify:
    /// - State root matches
    /// - Gas used is correct
    /// - Receipts root matches
    fn validate_block_post_execution_with_hashed_state(
        &self,
        state_updates: &HashedPostState,
        block: &RecoveredBlock<Self::Block>,
    ) -> Result<(), ConsensusError>;

    /// Validate payload attributes against parent header
    ///
    /// Ensures attributes are valid for building on top of parent:
    /// - Timestamp is after parent
    /// - Base fee is correctly calculated
    /// - Blob gas (if Cancun active)
    fn validate_payload_attributes_against_header(
        &self,
        attr: &Types::PayloadAttributes,
        header: &<Self::Block as Block>::Header,
    ) -> Result<(), InvalidPayloadAttributesError>;
}
```

**Example - Ethereum**:

```rust
#[derive(Clone, Debug)]
pub struct EthereumExecutionPayloadValidator<ChainSpec> {
    chain_spec: Arc<ChainSpec>,
}

impl<ChainSpec: EthereumHardforks> PayloadValidator<EthPayloadTypes>
    for EthereumExecutionPayloadValidator<ChainSpec>
{
    type Block = alloy_consensus::Block<TransactionSigned>;

    fn ensure_well_formed_payload(
        &self,
        payload: ExecutionData,
    ) -> Result<RecoveredBlock<Self::Block>, NewPayloadError> {
        let ExecutionData { payload, sidecar } = payload;
        let expected_hash = payload.block_hash();
        let sealed_block = payload.try_into_block_with_sidecar(&sidecar)?.seal_slow();

        // Verify hash
        if expected_hash != sealed_block.hash() {
            return Err(NewPayloadError::BlockHash {
                execution: sealed_block.hash(),
                consensus: expected_hash,
            });
        }

        // Shanghai validation (withdrawals)
        shanghai::ensure_well_formed_fields(
            sealed_block.body(),
            self.chain_spec.is_shanghai_active_at_timestamp(sealed_block.timestamp),
        )?;

        // Cancun validation (blobs)
        cancun::ensure_well_formed_fields(
            &sealed_block,
            sidecar.cancun(),
            self.chain_spec.is_cancun_active_at_timestamp(sealed_block.timestamp),
        )?;

        // Prague validation (execution requests)
        prague::ensure_well_formed_fields(
            sealed_block.body(),
            sidecar.prague(),
            self.chain_spec.is_prague_active_at_timestamp(sealed_block.timestamp),
        )?;

        sealed_block.try_recover().map_err(|e| NewPayloadError::Other(e.into()))
    }
}
```

### 4. Consensus - Block Validation

**Location**: `crates/consensus/consensus/src/lib.rs:35-125`

Core consensus validation traits:

```rust
/// Full consensus validation (pre + post execution)
pub trait FullConsensus<N: NodePrimitives>: Consensus<N::Block> {
    /// Validate block considering world state
    ///
    /// Verifies:
    /// - State root
    /// - Receipts root
    /// - Gas used
    /// - Blob gas used (Cancun+)
    fn validate_block_post_execution(
        &self,
        block: &RecoveredBlock<N::Block>,
        result: &BlockExecutionResult<N::Receipt>,
    ) -> Result<(), ConsensusError>;
}

/// Block and header validation (pre-execution)
pub trait Consensus<B: Block>: HeaderValidator<B::Header> {
    type Error;

    /// Validate body fields match header
    fn validate_body_against_header(
        &self,
        body: &B::Body,
        header: &SealedHeader<B::Header>,
    ) -> Result<(), Self::Error>;

    /// Validate block disregarding state
    ///
    /// Checks that don't require execution:
    /// - Header fields valid
    /// - Transactions root matches
    /// - Ommers hash (pre-Paris)
    /// - Withdrawals root (Shanghai+)
    fn validate_block_pre_execution(
        &self,
        block: &SealedBlock<B>,
    ) -> Result<(), Self::Error>;
}

/// Header validation
pub trait HeaderValidator<H = Header>: Debug + Send + Sync {
    /// Validate header standalone
    fn validate_header(&self, header: &SealedHeader<H>) -> Result<(), ConsensusError>;

    /// Validate header against parent
    ///
    /// Verifies:
    /// - Parent hash matches
    /// - Block number increments correctly
    /// - Timestamp is after parent
    /// - Gas limit within bounds
    /// - Base fee correctly calculated
    fn validate_header_against_parent(
        &self,
        header: &SealedHeader<H>,
        parent: &SealedHeader<H>,
    ) -> Result<(), ConsensusError>;
}
```

**Example - Ethereum**:

```rust
#[derive(Debug, Clone)]
pub struct EthereumConsensus {
    chain_spec: Arc<ChainSpec>,
}

impl HeaderValidator for EthereumConsensus {
    fn validate_header(&self, header: &SealedHeader) -> Result<(), ConsensusError> {
        validate_header_standalone(header, &self.chain_spec)
    }

    fn validate_header_against_parent(
        &self,
        header: &SealedHeader,
        parent: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        validate_header_against_parent(header, parent, &self.chain_spec)
    }
}

impl Consensus<Block> for EthereumConsensus {
    type Error = ConsensusError;

    fn validate_body_against_header(
        &self,
        body: &BlockBody,
        header: &SealedHeader,
    ) -> Result<(), Self::Error> {
        validate_body_against_header(body, header)
    }

    fn validate_block_pre_execution(&self, block: &SealedBlock) -> Result<(), Self::Error> {
        validate_block_pre_execution(block, &self.chain_spec)
    }
}
```

## Engine API Messages

### BeaconEngineMessage Enum

**Location**: `crates/engine/primitives/src/message.rs:148-167`

The CL communicates via these messages:

```rust
pub enum BeaconEngineMessage<Payload: PayloadTypes> {
    /// New block payload from consensus layer
    NewPayload {
        payload: Payload::ExecutionData,
        tx: oneshot::Sender<Result<PayloadStatus, BeaconOnNewPayloadError>>,
    },

    /// Fork choice update (may trigger block building)
    ForkchoiceUpdated {
        state: ForkchoiceState,
        payload_attrs: Option<Payload::PayloadAttributes>,
        version: EngineApiMessageVersion,
        tx: oneshot::Sender<RethResult<OnForkChoiceUpdated>>,
    },
}
```

### ForkchoiceState

Defines the canonical chain head:

```rust
pub struct ForkchoiceState {
    /// Hash of the head block
    pub head_block_hash: B256,
    /// Hash of the most recent finalized block
    pub finalized_block_hash: B256,
    /// Hash of the safe block
    pub safe_block_hash: B256,
}
```

### ConsensusEngineHandle - Message Sender

**Location**: `crates/engine/primitives/src/message.rs:198-260`

Convenient interface for sending Engine API messages:

```rust
pub struct ConsensusEngineHandle<Payload: PayloadTypes> {
    to_engine: UnboundedSender<BeaconEngineMessage<Payload>>,
}

impl<Payload: PayloadTypes> ConsensusEngineHandle<Payload> {
    /// Send newPayload message
    pub async fn new_payload(
        &self,
        payload: Payload::ExecutionData,
    ) -> Result<PayloadStatus, BeaconOnNewPayloadError> {
        let (tx, rx) = oneshot::channel();
        self.to_engine.send(BeaconEngineMessage::NewPayload { payload, tx })?;
        rx.await?
    }

    /// Send forkchoiceUpdated message
    pub async fn fork_choice_updated(
        &self,
        state: ForkchoiceState,
        payload_attrs: Option<Payload::PayloadAttributes>,
        version: EngineApiMessageVersion,
    ) -> Result<ForkchoiceUpdated, BeaconForkChoiceUpdateError> {
        let (tx, rx) = oneshot::channel();
        self.to_engine.send(BeaconEngineMessage::ForkchoiceUpdated {
            state,
            payload_attrs,
            version,
            tx,
        })?;
        rx.await?
    }
}
```

## Block Validation Pipeline

### Complete Validation Flow

**Location**: `crates/engine/tree/src/tree/payload_validator.rs:282-349`

The `BasicEngineValidator::validate_block_with_state()` method:

```
1. Convert Payload to Block
   ↓
2. Pre-execution Validation
   ├─ Consensus header validation
   ├─ Body-to-header validation
   └─ Header-to-parent validation
   ↓
3. State Preparation
   ├─ Build state provider
   └─ Apply historical block overlays
   ↓
4. Block Execution
   ├─ Execute all transactions
   └─ Collect receipts and gas used
   ↓
5. State Root Computation
   ├─ Calculate new state root
   └─ Parallel trie computation
   ↓
6. Post-execution Validation
   ├─ Verify state root matches
   ├─ Verify gas used
   ├─ Verify receipts root
   └─ Custom validation (e.g., OP Isthmus)
   ↓
7. Return Validated Block
```

### EngineApiTreeHandler - Main Engine Processor

**Location**: `crates/engine/tree/src/tree/mod.rs:235-279`

Manages block tree and processes Engine API messages:

```rust
pub struct EngineApiTreeHandler<N, P, T, V, C>
where
    N: NodePrimitives,
    T: PayloadTypes,
    C: ConfigureEvm<Primitives = N>,
{
    /// Database provider
    provider: P,
    /// Consensus rules
    consensus: Arc<dyn FullConsensus<N>>,
    /// Payload validator
    payload_validator: V,
    /// In-memory tree state
    state: EngineApiTreeState<N>,
    /// Payload builder handle
    payload_builder: PayloadBuilderHandle<T>,
    /// Canonical in-memory state
    canonical_in_memory_state: CanonicalInMemoryState<N>,
    /// EVM configuration
    evm_config: C,
    // ... other fields
}
```

### newPayload Processing

**Location**: `crates/engine/tree/src/tree/mod.rs:504-584`

```rust
fn on_new_payload(
    &mut self,
    payload: T::ExecutionData,
) -> Result<TreeOutcome<PayloadStatus>, InsertBlockFatalError> {
    // 1. Check for invalid ancestors
    if let Some(invalid) = self.find_invalid_ancestor(&payload) {
        let status = self.handle_invalid_ancestor_payload(payload, invalid)?;
        return Ok(TreeOutcome::new(status));
    }

    // 2. Route based on sync state
    let status = if self.backfill_sync_state.is_idle() {
        // Live sync: validate and insert immediately
        self.try_insert_payload(payload)?
    } else {
        // Backfill sync: buffer for later processing
        self.try_buffer_payload(payload)?
    };

    // 3. Make canonical if valid and is sync target
    if status.is_valid() && self.is_sync_target_head(block_hash) {
        outcome = outcome.with_event(TreeEvent::TreeAction(
            TreeAction::MakeCanonical { sync_target_head: block_hash }
        ));
    }

    Ok(outcome)
}
```

### forkchoiceUpdated Processing

**Location**: `crates/engine/tree/src/tree/mod.rs` (inferred from grep results)

```rust
fn on_forkchoice_updated(
    &mut self,
    state: ForkchoiceState,
    attrs: Option<T::PayloadAttributes>,
    version: EngineApiMessageVersion,
) -> ProviderResult<TreeOutcome<OnForkChoiceUpdated>> {
    // 1. Pre-validate forkchoice state
    if let Some(early_result) = self.validate_forkchoice_state(state)? {
        return Ok(TreeOutcome::new(early_result));
    }

    // 2. Check if already canonical (short path)
    if let Some(result) = self.handle_canonical_head(state, &attrs, version)? {
        return Ok(result);
    }

    // 3. Apply chain update (reorg/extension)
    if let Some(result) = self.apply_chain_update(state, &attrs, version)? {
        return Ok(result);
    }

    // 4. Request missing blocks
    self.handle_missing_block(state)
}
```

## Custom Implementations

### Example 1: Optimism Custom Validation

**Location**: `crates/optimism/node/src/engine.rs:27-165`

Optimism adds custom validation for Isthmus hardfork:

```rust
#[derive(Debug)]
pub struct OpEngineValidator<P, Tx, ChainSpec> {
    inner: OpExecutionPayloadValidator<ChainSpec>,
    provider: P,
    hashed_addr_l2tol1_msg_passer: B256,
    phantom: PhantomData<Tx>,
}

impl<P, Tx, ChainSpec, Types> PayloadValidator<Types>
    for OpEngineValidator<P, Tx, ChainSpec>
where
    P: StateProviderFactory + Unpin + 'static,
    Tx: SignedTransaction + Unpin + 'static,
    ChainSpec: OpHardforks + Send + Sync + 'static,
    Types: PayloadTypes<ExecutionData = OpExecutionData>,
{
    type Block = alloy_consensus::Block<Tx>;

    fn ensure_well_formed_payload(
        &self,
        payload: OpExecutionData,
    ) -> Result<RecoveredBlock<Self::Block>, NewPayloadError> {
        // Delegate to inner validator
        let sealed_block = self.inner
            .ensure_well_formed_payload(payload)
            .map_err(NewPayloadError::other)?;

        sealed_block
            .try_recover()
            .map_err(|e| NewPayloadError::Other(e.into()))
    }

    fn validate_block_post_execution_with_hashed_state(
        &self,
        state_updates: &HashedPostState,
        block: &RecoveredBlock<Self::Block>,
    ) -> Result<(), ConsensusError> {
        // Optimism-specific: validate withdrawals storage root (Isthmus+)
        if self.chain_spec.is_isthmus_active_at_timestamp(block.timestamp) {
            let state = self.provider.state_by_block_hash(block.parent_hash)?;
            let predeploy_storage_updates = state_updates
                .storages
                .get(&self.hashed_addr_l2tol1_msg_passer)
                .cloned()
                .unwrap_or_default();

            // Compute and verify withdrawals storage root
            // ... validation logic
        }

        Ok(())
    }
}
```

### Example 2: Custom EngineTypes

```rust
#[derive(Debug, Clone)]
pub struct MyCustomEngineTypes;

impl PayloadTypes for MyCustomEngineTypes {
    type ExecutionData = MyExecutionData;
    type BuiltPayload = MyBuiltPayload;
    type PayloadAttributes = MyPayloadAttributes;
    type PayloadBuilderAttributes = MyPayloadBuilderAttributes;

    fn block_to_payload(block: SealedBlock<...>) -> Self::ExecutionData {
        // Custom conversion logic
        MyExecutionData::from_block(block)
    }
}

impl EngineTypes for MyCustomEngineTypes {
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = MyCustomPayloadEnvelopeV3;  // Custom V3
    type ExecutionPayloadEnvelopeV4 = ExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = ExecutionPayloadEnvelopeV5;
}
```

### Example 3: Custom Consensus Rules

```rust
pub struct MyConsensus {
    ethereum_consensus: EthereumConsensus,
    custom_validators: Vec<Address>,
}

impl Consensus<Block> for MyConsensus {
    type Error = ConsensusError;

    fn validate_header(&self, header: &SealedHeader) -> Result<(), Self::Error> {
        // Run Ethereum validation
        self.ethereum_consensus.validate_header(header)?;

        // Add custom validation (e.g., PoA signer check)
        let signer = self.recover_signer(header)?;
        if !self.custom_validators.contains(&signer) {
            return Err(ConsensusError::InvalidSigner);
        }

        Ok(())
    }

    fn validate_body_against_header(
        &self,
        body: &BlockBody,
        header: &SealedHeader,
    ) -> Result<(), Self::Error> {
        // Delegate to Ethereum
        self.ethereum_consensus.validate_body_against_header(body, header)
    }

    fn validate_block_pre_execution(
        &self,
        block: &SealedBlock,
    ) -> Result<(), Self::Error> {
        // Delegate to Ethereum
        self.ethereum_consensus.validate_block_pre_execution(block)
    }
}
```

## Integration Points

### 1. Node Builder Integration

**Location**: `crates/node/builder/src/components/consensus.rs`

```rust
pub trait ConsensusBuilder<Node: FullNodeTypes>: Send {
    type Consensus: FullConsensus<Node::Primitives, Error = ConsensusError> + Clone + Unpin + 'static;

    fn build_consensus(self, ctx: &BuilderContext<Node>) -> impl Future<Output = eyre::Result<Self::Consensus>> + Send;
}

// Ethereum implementation
pub struct EthereumConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for EthereumConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: EthereumHardforks>>,
{
    type Consensus = Arc<EthereumConsensus>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(EthereumConsensus::new(ctx.chain_spec())))
    }
}
```

### 2. Engine Validator Integration

**Location**: `crates/node/builder/src/components/mod.rs`

Combine PayloadValidator with EngineTypes:

```rust
pub trait EngineValidatorBuilder<Node: FullNodeComponents>: Send {
    type Validator: PayloadValidator<<Node::Types as NodeTypes>::Payload> + Clone + 'static;

    fn build(
        self,
        ctx: &AddOnsContext<'_, Node>,
    ) -> Self::Validator;
}

// Basic implementation
pub struct BasicEngineValidatorBuilder<Inner> {
    inner: Inner,
}

impl<Node, Inner> EngineValidatorBuilder<Node> for BasicEngineValidatorBuilder<Inner>
where
    Node: FullNodeComponents,
    Inner: EngineValidatorBuilder<Node>,
{
    type Validator = BasicEngineValidator<...>;

    fn build(self, ctx: &AddOnsContext<'_, Node>) -> Self::Validator {
        BasicEngineValidator::new(
            ctx.node.provider().clone(),
            ctx.node.evm_config().clone(),
            self.inner.build(ctx),
        )
    }
}
```

## Key Takeaways

1. **Layered Validation**: Pre-execution → Execution → Post-execution
2. **Trait-Based Customization**: Override only what you need
3. **Version Support**: Handle multiple Engine API versions gracefully
4. **State Management**: In-memory tree for fast validation, async DB persistence
5. **Type Safety**: Generic over primitives for chain-specific types

## Next Steps

- Read the [Payload Builder Guide](./PAYLOAD_BUILDER_GUIDE.md) to understand block building
- Read the [Chain Spec & EVM Guide](./CHAINSPEC_EVM_GUIDE.md) for hardfork configuration
- Study the Optimism implementation for L2-specific patterns
- Experiment with custom consensus rules for private networks
