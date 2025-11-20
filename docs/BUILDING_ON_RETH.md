# Building on Reth: Extensibility Guide

This guide explains how to extend Reth to build custom Ethereum-like blockchains and execution clients. Reth's architecture is designed for modularity, allowing you to customize nearly every component while reusing the robust core infrastructure.

## Overview

Reth's extensibility model is built around **traits** that define clear interfaces for each component. By implementing these traits with your custom logic, you can:

- Build custom L2s and rollups (like Optimism)
- Create alternative execution environments
- Implement custom consensus rules
- Extend RPC APIs with chain-specific methods
- Modify transaction validation logic
- Customize payload building strategies

## Core Extensibility Traits

### 1. NodeTypes - Configuring Your Node's Type System

**Location**: `crates/node/types/src/lib.rs:27-36`

The `NodeTypes` trait is the foundation of node customization. It defines the fundamental types your node will use:

```rust
pub trait NodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    /// The node's primitive types (blocks, transactions, receipts, headers)
    type Primitives: NodePrimitives;

    /// Chain specification (genesis, hardforks, consensus parameters)
    type ChainSpec: EthChainSpec<Header = <Self::Primitives as NodePrimitives>::BlockHeader>;

    /// Storage backend configuration
    type Storage: Default + Send + Sync + Unpin + Debug + 'static;

    /// Payload types for block building
    type Payload: PayloadTypes<BuiltPayload: BuiltPayload<Primitives = Self::Primitives>>;
}
```

**When to customize**: Creating a custom L2, using non-standard primitives, or implementing a new consensus mechanism.

**Example** (from Optimism - `crates/optimism/node/src/node.rs:76-87`):
```rust
pub trait OpNodeTypes: NodeTypes<
    Payload = OpEngineTypes,
    ChainSpec: OpHardforks + Hardforks,
    Primitives = OpPrimitives,
> {}
```

### 2. ConfigureEvm - Customizing Execution Environment

**Location**: `crates/evm/evm/src/lib.rs:184-456`

The `ConfigureEvm` trait controls how the EVM executes transactions and builds blocks. This is the primary trait for customizing execution behavior.

```rust
pub trait ConfigureEvm: Clone + Debug + Send + Sync + Unpin {
    /// The primitives type used by the EVM
    type Primitives: NodePrimitives;

    /// Error type for EVM configuration
    type Error: Error + Send + Sync + 'static;

    /// Context for configuring next block (timestamp, fee recipient, etc.)
    type NextBlockEnvCtx: Debug + Clone;

    /// Factory for creating block executors
    type BlockExecutorFactory: BlockExecutorFactory;

    /// Block assembler for constructing final blocks
    type BlockAssembler: BlockAssembler;

    /// Configure EVM environment for a block header
    fn evm_env(&self, header: &HeaderTy<Self::Primitives>)
        -> Result<EvmEnvFor<Self>, Self::Error>;

    /// Configure EVM environment for next block (used in block building)
    fn next_evm_env(
        &self,
        parent: &HeaderTy<Self::Primitives>,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error>;

    // ... more methods for creating executors and builders
}
```

**Key Capabilities**:
- **Block Execution**: Execute complete blocks with all transactions (`executor()`)
- **Block Building**: Build new blocks with custom transaction selection (`builder_for_next_block()`)
- **EVM Customization**: Modify gas rules, precompiles, or execution logic

**Example Usage** (from docs - `crates/evm/evm/src/lib.rs:95-106`):
```rust
// Execute a received block
let mut executor = evm_config.executor(state_db);
let output = executor.execute(&block)?;

// Build a new block
let mut builder = evm_config.builder_for_next_block(
    &mut state_db,
    &parent_header,
    attributes
)?;

// Apply pre-execution changes (e.g., beacon root update)
builder.apply_pre_execution_changes()?;

// Execute transactions
for tx in pending_transactions {
    builder.execute_transaction(tx)?;
}

// Finish and get the built block
let outcome = builder.finish(state_provider)?;
```

### 3. NodeComponents - Assembling the Full Node

**Location**: `crates/node/builder/src/components/mod.rs:38-69`

The `NodeComponents` trait brings together all major components of a running node:

```rust
pub trait NodeComponents<T: FullNodeTypes>: Clone + Debug + Unpin + Send + Sync + 'static {
    /// Transaction pool implementation
    type Pool: TransactionPool;

    /// EVM configuration
    type Evm: ConfigureEvm;

    /// Consensus implementation
    type Consensus: FullConsensus;

    /// Network implementation
    type Network: FullNetwork;

    fn pool(&self) -> &Self::Pool;
    fn evm_config(&self) -> &Self::Evm;
    fn consensus(&self) -> &Self::Consensus;
    fn network(&self) -> &Self::Network;
    fn payload_builder_handle(&self) -> &PayloadBuilderHandle;
}
```

**When to customize**: You typically don't implement this directly. Instead, use `ComponentsBuilder` to configure individual components.

### 4. Storage and State Providers

**Location**: `crates/storage/storage-api/src/`

Key storage traits for reading and writing blockchain data:

- `BlockReader` - Read blocks, headers, and block bodies
- `BlockWriter` - Write blocks to storage
- `StateProvider` - Access account state at specific blocks
- `ReceiptProvider` - Access transaction receipts
- `DatabaseProvider` - Main interface combining all storage capabilities

**Example**:
```rust
// Reading block data
let block = provider.block_by_number(block_number)?;
let header = provider.header_by_number(block_number)?;

// Accessing state
let account = state_provider.basic_account(address)?;
let storage_value = state_provider.storage(address, storage_key)?;

// Reading receipts
let receipts = provider.receipts_by_block(block_hash)?;
```

## Real-World Example: Optimism

The best example of extending Reth is the Optimism implementation. Here's how it customizes key components:

### Custom Node Types (`crates/optimism/node/src/node.rs`)

```rust
// Define Optimism-specific node types
pub trait OpNodeTypes: NodeTypes<
    Payload = OpEngineTypes,           // Custom payload with L1 data
    ChainSpec: OpHardforks + Hardforks, // Op-specific forks
    Primitives = OpPrimitives,         // Deposit transactions support
> {}
```

### Custom EVM Config (`crates/optimism/evm/`)

Optimism customizes execution to handle:
- Deposit transactions (L1 → L2)
- Custom gas calculations
- L1 data fee computation
- Different precompiles

### Custom Transaction Pool (`crates/optimism/txpool/`)

Optimism's transaction validator handles deposit transactions differently:
```rust
pub struct OpTransactionValidator {
    inner: EthTransactionValidator,
    // Additional OP-specific validation logic
}
```

## Common Extension Patterns

### Pattern 1: Adding Custom Precompiles

```rust
// Implement custom EVM factory with additional precompiles
impl EvmFactory for MyEvmFactory {
    fn create_evm(&self, db: DB, env: EvmEnv) -> Evm {
        let mut evm = self.base_factory.create_evm(db, env);

        // Add custom precompile at address 0x100
        evm.handler.precompiles.insert(
            address!("0000000000000000000000000000000000000100"),
            MyCustomPrecompile::new(),
        );

        evm
    }
}
```

### Pattern 2: Custom Transaction Validation

```rust
// Implement custom transaction validator
pub struct MyTransactionValidator<C> {
    chain_spec: Arc<C>,
    // Your custom validation state
}

impl<C> TransactionValidator for MyTransactionValidator<C>
where
    C: EthChainSpec,
{
    fn validate_transaction(
        &self,
        tx: &Transaction,
    ) -> Result<(), ValidationError> {
        // Your custom validation logic
        if !self.is_allowed_sender(&tx.sender) {
            return Err(ValidationError::Forbidden);
        }

        // Standard validation
        self.validate_standard(tx)
    }
}
```

### Pattern 3: Custom RPC Methods

```rust
// Define your custom RPC trait
#[rpc(server, namespace = "mychain")]
pub trait MyChainApi {
    #[method(name = "getCustomData")]
    async fn get_custom_data(&self, block_number: u64) -> RpcResult<CustomData>;
}

// Implement it
pub struct MyChainApiImpl<Provider> {
    provider: Provider,
}

impl<Provider> MyChainApiServer for MyChainApiImpl<Provider>
where
    Provider: BlockReader + StateProvider,
{
    async fn get_custom_data(&self, block_number: u64) -> RpcResult<CustomData> {
        let block = self.provider.block_by_number(block_number)?;
        // Process and return custom data
        Ok(extract_custom_data(block))
    }
}
```

### Pattern 4: Custom Chain Specification

```rust
// Define custom hardforks
#[derive(Clone, Copy, Debug)]
pub enum MyChainHardfork {
    CustomUpgrade1,
    CustomUpgrade2,
}

// Implement ChainSpec with custom hardforks
pub struct MyChainSpec {
    base: ChainSpec,
    custom_forks: HashMap<MyChainHardfork, u64>,
}

impl EthChainSpec for MyChainSpec {
    fn fork_id(&self, head: &Head) -> ForkId {
        // Custom fork ID calculation
    }
}

impl Hardforks for MyChainSpec {
    fn is_fork_active_at_block(&self, fork: Hardfork, block: u64) -> bool {
        // Check activation of standard and custom forks
    }
}
```

## Step-by-Step: Building a Custom Chain

### Step 1: Define Your Node Types

```rust
use reth_node_api::NodeTypes;
use reth_primitives::EthPrimitives;

#[derive(Clone, Debug, Default)]
pub struct MyChainTypes;

impl NodeTypes for MyChainTypes {
    type Primitives = EthPrimitives; // Or your custom primitives
    type ChainSpec = MyChainSpec;
    type Storage = MyStorage;
    type Payload = MyPayloadTypes;
}
```

### Step 2: Configure EVM

```rust
#[derive(Clone, Debug)]
pub struct MyEvmConfig {
    chain_spec: Arc<MyChainSpec>,
}

impl ConfigureEvm for MyEvmConfig {
    type Primitives = EthPrimitives;
    type Error = MyEvmError;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = MyExecutorFactory;
    type BlockAssembler = MyBlockAssembler;

    fn evm_env(&self, header: &Header) -> Result<EvmEnv, Self::Error> {
        // Configure EVM environment based on your chain rules
    }

    // Implement other required methods...
}
```

### Step 3: Build Components

```rust
use reth_node_builder::components::ComponentsBuilder;

pub struct MyComponentsBuilder;

impl<Node> ComponentsBuilder<Node> for MyComponentsBuilder
where
    Node: FullNodeTypes<Types = MyChainTypes>,
{
    type Components = Components<
        Node,
        MyNetwork,
        MyTxPool,
        MyEvmConfig,
        MyConsensus,
    >;

    async fn build_components(
        &self,
        ctx: &BuilderContext<Node>,
    ) -> Result<Self::Components, eyre::Error> {
        // Build and return all components
        Ok(Components {
            transaction_pool: build_tx_pool(ctx)?,
            evm_config: MyEvmConfig::new(ctx.chain_spec()),
            consensus: MyConsensus::new(ctx.chain_spec()),
            network: build_network(ctx).await?,
            payload_builder_handle: build_payload_service(ctx)?,
        })
    }
}
```

### Step 4: Assemble and Launch Node

```rust
use reth_node_builder::{Node, NodeBuilder};

#[derive(Debug, Clone, Default)]
pub struct MyChainNode;

impl Node for MyChainNode {
    type Types = MyChainTypes;
    type ComponentsBuilder = MyComponentsBuilder;
    type AddOns = MyAddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        MyComponentsBuilder
    }
}

// Launch the node
#[tokio::main]
async fn main() -> eyre::Result<()> {
    let node = NodeBuilder::new()
        .with_types::<MyChainTypes>()
        .with_components(MyComponentsBuilder)
        .launch()
        .await?;

    node.wait_for_shutdown().await
}
```

## Testing Your Extensions

### Unit Testing Components

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use reth_evm::test_utils::StateProviderTest;

    #[test]
    fn test_custom_evm_config() {
        let config = MyEvmConfig::new(Arc::new(test_chain_spec()));
        let header = test_header();

        let evm_env = config.evm_env(&header).unwrap();

        assert_eq!(evm_env.cfg.chain_id, 1234);
        // Test your custom configuration
    }

    #[tokio::test]
    async fn test_custom_transaction_validation() {
        let validator = MyTransactionValidator::new(chain_spec);
        let tx = test_transaction();

        let result = validator.validate_transaction(&tx);

        assert!(result.is_ok());
    }
}
```

### Integration Testing

```rust
#[tokio::test]
async fn test_full_node() {
    let node = NodeBuilder::new()
        .with_types::<MyChainTypes>()
        .with_components(MyComponentsBuilder)
        .testing_mode()
        .launch()
        .await
        .unwrap();

    // Test node functionality
    let provider = node.provider();
    let block = provider.block_by_number(0).unwrap();

    assert!(block.is_some());
}
```

## Best Practices

### 1. Reuse Standard Components When Possible

Don't reinvent the wheel. If standard Ethereum behavior works for your use case, reuse it:

```rust
// Good: Reuse standard executor, customize only what's needed
pub struct MyEvmConfig {
    base: EthEvmConfig,  // Reuse standard Ethereum config
    custom_rules: MyCustomRules,
}
```

### 2. Keep Trait Bounds Minimal

Make your types as generic as possible to maximize reusability:

```rust
// Good: Generic over any chain spec implementing required traits
pub struct MyComponent<C>
where
    C: EthChainSpec,
{
    chain_spec: Arc<C>,
}

// Avoid: Hardcoded to specific type
pub struct MyComponent {
    chain_spec: Arc<ChainSpec>,  // Too specific
}
```

### 3. Use Type Aliases for Clarity

```rust
// Define helper type aliases
pub type MyNodeTypes = AnyNodeTypes<
    EthPrimitives,
    MyChainSpec,
    MyStorage,
    MyPayloadTypes,
>;

pub type MyProvider<DB> = DatabaseProvider<DB, MyNodeTypes>;
```

### 4. Follow Reth's Error Handling Patterns

Use proper error types with good context:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyChainError {
    #[error("Invalid custom field: {0}")]
    InvalidField(String),

    #[error(transparent)]
    Execution(#[from] BlockExecutionError),
}
```

### 5. Add Comprehensive Documentation

Document your traits and types thoroughly:

```rust
/// Custom EVM configuration for MyChain.
///
/// This config extends standard Ethereum execution with:
/// - Custom precompile at 0x100 for X functionality
/// - Modified gas calculation for Y operations
/// - Special handling of Z transaction types
///
/// # Example
///
/// ```rust
/// let config = MyEvmConfig::new(chain_spec);
/// let executor = config.executor(db);
/// ```
pub struct MyEvmConfig { ... }
```

## Resources

### Key Source Files to Study

- **Node Types**: `crates/node/types/src/lib.rs`
- **EVM Config**: `crates/evm/evm/src/lib.rs`
- **Components**: `crates/node/builder/src/components/`
- **Optimism Example**: `crates/optimism/`
- **Node Builder**: `crates/node/builder/src/`

### Running Examples

Check Reth's examples directory:
```bash
# Run example node
cargo run --example custom-engine-types

# Run example with custom EVM
cargo run --example custom-evm

# Study Optimism implementation
cargo doc --package reth-optimism-node --open
```

### Getting Help

- **Documentation**: Run `cargo doc --open --document-private-items` to view full API docs
- **Examples**: Browse `examples/` directory for working code
- **Community**: Ask questions in Reth's GitHub Discussions or Telegram
- **Source Code**: When in doubt, read the source - it's well-documented

## Next Steps

1. **Study the Optimism implementation** in `crates/optimism/` - it's the canonical example of extending Reth
2. **Read the node builder docs** to understand the orchestration layer
3. **Start small** - begin by customizing one component (like adding a custom RPC method)
4. **Run the examples** to see working implementations
5. **Contribute back** - if you build something useful, consider contributing it upstream

---

For more detailed architectural information, see:
- [Design Documentation](https://github.com/okx/reth/tree/dev/docs/design/README.md)
- [Crate Overview](https://github.com/okx/reth/tree/dev/docs/crates/README.md)
- [Repository Structure](https://github.com/okx/reth/tree/dev/docs/repo/README.md)
- [Development Workflow](https://github.com/okx/reth/tree/dev/docs/workflow.md)
