# Reth Extension Guide

This guide provides comprehensive documentation for extending the Reth codebase with custom implementations. Reth is designed to be highly modular, allowing you to customize every major component of an Ethereum execution client.

## Table of Contents

1. [Overview](#overview)
2. [Quick Start](#quick-start)
3. [Extension Points](#extension-points)
4. [Detailed Guides](#detailed-guides)
5. [Examples](#examples)

## Overview

Reth's architecture is built around **traits** that define interfaces for all major components. You can extend Reth by:

- **Creating custom nodes** with different component configurations
- **Implementing custom consensus rules** for L2s or private chains
- **Building custom payload builders** with specialized transaction ordering
- **Defining custom chain specifications** with unique hardforks and parameters
- **Customizing EVM behavior** with custom precompiles or opcodes
- **Adding RPC extensions** for chain-specific APIs
- **Writing ExExes (Execution Extensions)** for indexing or state extraction

## Quick Start

### 1. Creating a Custom Node

```rust
use reth::builder::{NodeBuilder, NodeConfig};
use reth_node_ethereum::EthereumNode;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let node_config = NodeConfig::default();

    let handle = NodeBuilder::new(node_config)
        .with_database(db)
        .with_types::<EthereumNode>()
        .with_components(EthereumNode::components())
        .with_add_ons(EthereumAddOns::default())
        .launch()
        .await?;

    handle.node_exit_future.await
}
```

### 2. Extending with Custom Components

```rust
// Define custom pool builder
pub struct MyPoolBuilder;

impl<Node> PoolBuilder<Node> for MyPoolBuilder
where
    Node: FullNodeTypes,
{
    type Pool = MyCustomPool;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<Self::Pool> {
        Ok(MyCustomPool::new(ctx.provider().clone()))
    }
}

// Use in node
let components = ComponentsBuilder::default()
    .node_types::<MyNode>()
    .pool(MyPoolBuilder)  // Custom pool
    .executor(EthereumExecutorBuilder::default())
    .payload(BasicPayloadServiceBuilder::default())
    .network(EthereumNetworkBuilder::default())
    .consensus(EthereumConsensusBuilder::default());
```

## Extension Points

### Core Components

| Component | Trait | Purpose | Example Use Cases |
|-----------|-------|---------|-------------------|
| **Node Types** | `NodeTypes` | Define primitives, chain spec, storage | Custom L2, private chain |
| **Transaction Pool** | `TransactionPool` | Manage pending transactions | Priority ordering, censorship resistance |
| **Consensus** | `Consensus`, `FullConsensus` | Validate blocks | PoA, custom consensus rules |
| **Network** | `NetworkBuilder` | P2P networking | Custom protocols, private networks |
| **Payload Builder** | `PayloadBuilder` | Build new blocks | MEV, sequencer integration |
| **EVM Config** | `ConfigureEvm` | EVM execution settings | Custom precompiles, gas schedules |
| **Chain Spec** | `EthChainSpec` | Chain parameters | Custom hardforks, genesis |

### Additional Extensions

| Extension | Trait/Pattern | Purpose |
|-----------|--------------|---------|
| **RPC Modules** | `RpcModuleBuilder` | Add custom RPC endpoints |
| **CLI Commands** | `clap::Subcommand` | Custom node commands |
| **ExExes** | Execution Extensions | Index data, extract state |
| **Validators** | `PayloadValidator` | Custom payload validation |
| **Launchers** | `Launcher` | Custom node initialization |

## Detailed Guides

### 1. [Node Builder Guide](./NODE_BUILDER_GUIDE.md)
Learn how to create custom nodes with different component configurations.

**Topics covered:**
- Understanding the node builder state machine
- NodeTypes and FullNodeTypes traits
- Component builders (pool, executor, network, consensus, payload)
- Node add-ons for RPC and services
- Complete examples (Ethereum, Optimism)

### 2. [Engine & Consensus Guide](./ENGINE_CONSENSUS_GUIDE.md)
Understand the Engine API and consensus validation.

**Topics covered:**
- EngineTypes and PayloadTypes traits
- Engine API messages (newPayload, forkchoiceUpdated)
- Block validation pipeline
- Custom consensus implementations
- Payload validation

### 3. [Payload Builder Guide](./PAYLOAD_BUILDER_GUIDE.md)
Build custom block building logic.

**Topics covered:**
- PayloadBuilder trait
- Transaction selection and ordering
- Payload attributes and configuration
- Integration with Engine API
- Custom builder implementations

### 4. [Chain Spec & EVM Guide](./CHAINSPEC_EVM_GUIDE.md)
Define custom chains and EVM behavior.

**Topics covered:**
- ChainSpec structure and configuration
- Hardfork management
- ConfigureEvm trait
- Custom precompiles and opcodes
- Base fee and blob parameters

### 5. [Database & Storage Guide](./DATABASE_GUIDE.md)
Understand Reth's hybrid storage architecture and work with data.

**Topics covered:**
- MDBX database (hot data)
- Static files (cold data)
- Database traits and tables
- Provider pattern
- Reading and writing data
- Custom tables

## Examples

### Example 1: Custom L2 Node (Optimism-style)

```rust
#[derive(Debug, Clone)]
pub struct MyL2Node {
    pub sequencer_url: String,
}

impl NodeTypes for MyL2Node {
    type Primitives = EthPrimitives;
    type ChainSpec = MyL2ChainSpec;
    type Storage = EthStorage;
    type Payload = MyL2EngineTypes;
}

impl<N> Node<N> for MyL2Node
where
    N: FullNodeTypes<Types: NodeTypes<Primitives = EthPrimitives>>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        MyL2PoolBuilder,
        BasicPayloadServiceBuilder<MyL2PayloadBuilder>,
        EthereumNetworkBuilder,
        EthereumExecutorBuilder,
        MyL2ConsensusBuilder,
    >;

    type AddOns = MyL2AddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        ComponentsBuilder::default()
            .pool(MyL2PoolBuilder::new(self.sequencer_url.clone()))
            .payload(BasicPayloadServiceBuilder::new(
                MyL2PayloadBuilder::new(self.sequencer_url.clone())
            ))
            .consensus(MyL2ConsensusBuilder::default())
            // ... other components
    }
}
```

### Example 2: Custom Payload Builder with MEV

```rust
pub struct MevPayloadBuilder {
    mev_relay_url: String,
    fallback_builder: EthereumPayloadBuilder,
}

impl PayloadBuilder for MevPayloadBuilder {
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        // Try to fetch block from MEV relay
        if let Ok(mev_payload) = self.fetch_from_relay(&args.config.attributes) {
            return Ok(BuildOutcome::Better { payload: mev_payload });
        }

        // Fall back to local building
        self.fallback_builder.try_build(args)
    }
}
```

### Example 3: Custom Consensus for PoA

```rust
pub struct PoAConsensus {
    authorized_signers: HashSet<Address>,
}

impl Consensus<Block> for PoAConsensus {
    type Error = ConsensusError;

    fn validate_header(&self, header: &SealedHeader) -> Result<(), Self::Error> {
        // Extract signer from extra data
        let signer = self.extract_signer(header)?;

        // Verify signer is authorized
        if !self.authorized_signers.contains(&signer) {
            return Err(ConsensusError::InvalidSigner);
        }

        Ok(())
    }
}
```

### Example 4: Custom Chain Spec with New Hardfork

```rust
use reth_chainspec::{ChainSpec, hardfork};

// Define custom hardfork
hardfork!(
    MyHardforks {
        CustomUpgrade1,
        CustomUpgrade2,
    }
);

pub struct MyChainSpec {
    pub inner: ChainSpec,
}

impl MyChainSpec {
    pub fn new() -> Self {
        let mut inner = ChainSpec::builder()
            .chain(Chain::from_id(12345))
            .genesis(Genesis::default())
            .paris_activated()
            .build();

        // Add custom hardforks
        inner.hardforks.insert(
            MyHardforks::CustomUpgrade1,
            ForkCondition::Timestamp(1700000000),
        );

        Self { inner }
    }
}
```

## Project Structure for Extensions

When building a custom implementation, organize your code like this:

```
my-custom-reth/
├── bin/
│   └── src/
│       └── main.rs              # CLI entry point
├── crates/
│   ├── chainspec/
│   │   └── src/
│   │       └── lib.rs           # Custom ChainSpec
│   ├── cli/
│   │   └── src/
│   │       └── lib.rs           # CLI configuration
│   ├── consensus/
│   │   └── src/
│   │       └── lib.rs           # Consensus implementation
│   ├── evm/
│   │   └── src/
│   │       └── lib.rs           # EVM configuration
│   ├── node/
│   │   └── src/
│   │       ├── node.rs          # Node type definition
│   │       ├── builder.rs       # Component builders
│   │       └── engine.rs        # Engine types
│   ├── payload/
│   │   └── src/
│   │       └── lib.rs           # Payload builder
│   └── primitives/
│       └── src/
│           └── lib.rs           # Custom types
└── Cargo.toml
```

## Best Practices

1. **Start with Ethereum**: Extend `EthereumNode` before building from scratch
2. **Reuse Components**: Only customize what you need to change
3. **Follow Patterns**: Study Optimism implementation for L2 patterns
4. **Type Safety**: Leverage Rust's type system for compile-time guarantees
5. **Test Thoroughly**: Write integration tests for custom components
6. **Document**: Add doc comments explaining custom behavior
7. **Version Carefully**: Pin reth dependencies to avoid breaking changes

## Common Patterns

### Pattern 1: Wrapping Ethereum Components

```rust
pub struct MyConsensus {
    inner: EthereumConsensus,
    custom_rules: MyRules,
}

impl Consensus<Block> for MyConsensus {
    fn validate_header(&self, header: &SealedHeader) -> Result<(), Self::Error> {
        // Run standard Ethereum validation
        self.inner.validate_header(header)?;

        // Add custom rules
        self.custom_rules.validate(header)?;

        Ok(())
    }
}
```

### Pattern 2: Delegating to Inner Types

```rust
pub struct MyChainSpec {
    pub inner: ChainSpec,
}

impl EthChainSpec for MyChainSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        self.inner.chain()  // Delegate to inner
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()  // Delegate
    }

    // Override only what's different
    fn is_optimism(&self) -> bool {
        true  // Custom behavior
    }
}
```

### Pattern 3: Builder Composition

```rust
// Compose multiple transaction sources
let txs = PayloadTransactionsChain::new([
    (sequencer_txs, 1000),     // Up to 1000 sequencer txs
    (priority_pool_txs, 500),  // Then 500 priority txs
    (mempool_txs, u64::MAX),   // Then rest from mempool
]);
```

## Debugging Tips

1. **Enable Logging**: Use `RUST_LOG=debug` to see component initialization
2. **Check Type Errors**: Trait bounds can be complex; read errors carefully
3. **Inspect State**: Use RPC to query node state during development
4. **Unit Test Components**: Test each component independently
5. **Use Examples**: Run reth examples to see patterns in action

## Resources

- **Reth Book**: https://paradigmxyz.github.io/reth
- **API Docs**: https://docs.rs/reth
- **Examples**: `/examples` directory in reth repository
- **Optimism Implementation**: `/crates/optimism` for real-world L2 example
- **Discord**: Join Reth Discord for community support

## Getting Help

- Open an issue: https://github.com/paradigmxyz/reth/issues
- Ask on Discord: https://discord.gg/reth
- Check existing implementations: Study Ethereum and Optimism nodes
- Read the source: Code is well-documented with inline comments

## Next Steps

1. Read the [Node Builder Guide](./NODE_BUILDER_GUIDE.md) to understand the foundation
2. Choose which components you need to customize
3. Study the relevant detailed guide
4. Start with a simple extension and iterate
5. Contribute back to the community!
