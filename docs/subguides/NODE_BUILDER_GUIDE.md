# Node Builder Guide

This guide explains how to use Reth's node builder architecture to create custom nodes with different component configurations.

## Table of Contents

1. [Overview](#overview)
2. [Core Traits](#core-traits)
3. [Builder State Machine](#builder-state-machine)
4. [Component Builders](#component-builders)
5. [Complete Examples](#complete-examples)
6. [Customization Patterns](#customization-patterns)

## Overview

Reth's node builder uses a **type-state pattern** to enforce correct configuration order at compile time. The builder progresses through three main states:

1. **Initial State**: `NodeBuilder` - Only config and database
2. **Types Configured**: `NodeBuilderWithTypes<T>` - Node type system defined
3. **Components Ready**: `NodeBuilderWithComponents<T, CB, AO>` - All components configured

### Key Concepts

- **NodeTypes**: Stateless type configuration (primitives, chain spec, storage types)
- **FullNodeTypes**: Extends NodeTypes with database and provider
- **NodeComponents**: Actual component instances (pool, network, consensus, etc.)
- **NodeComponentsBuilder**: Builds components from configuration
- **NodeAddOns**: Additional services launched after core components (RPC, monitoring)

## Core Traits

### 1. NodeTypes - Type Configuration

**Location**: `crates/node/types/src/lib.rs`

Defines the fundamental type system for your node:

```rust
pub trait NodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    /// Primitive types (Block, Transaction, Receipt, etc.)
    type Primitives: NodePrimitives;

    /// Chain specification type
    type ChainSpec: EthChainSpec<Header = <Self::Primitives as NodePrimitives>::BlockHeader>;

    /// Storage type for writing primitives
    type Storage: Default + Send + Sync + Unpin + Debug + 'static;

    /// Engine/payload types
    type Payload: PayloadTypes<BuiltPayload: BuiltPayload<Primitives = Self::Primitives>>;
}
```

**Example Implementation**:

```rust
#[derive(Debug, Default, Clone)]
pub struct MyCustomNode;

impl NodeTypes for MyCustomNode {
    type Primitives = EthPrimitives;  // Standard Ethereum types
    type ChainSpec = ChainSpec;        // Standard chain spec
    type Storage = EthStorage;         // Standard storage
    type Payload = EthEngineTypes;     // Standard engine types
}
```

### 2. FullNodeTypes - Adding Database and Provider

**Location**: `crates/node/api/src/node.rs`

Extends NodeTypes with stateful components:

```rust
pub trait FullNodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    /// Node's type configuration
    type Types: NodeTypes;

    /// Database type
    type DB: Database + DatabaseMetrics + Clone + Unpin + 'static;

    /// Provider for state access
    type Provider: FullProvider<NodeTypesWithDBAdapter<Self::Types, Self::DB>>;
}
```

### 3. NodeComponents - Component Instances

**Location**: `crates/node/builder/src/components/mod.rs`

Holds actual running components:

```rust
pub trait NodeComponents<T: FullNodeTypes>: Clone + Debug + Unpin + Send + Sync + 'static {
    /// Transaction pool
    type Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<T::Types>>> + Unpin;

    /// EVM configuration
    type Evm: ConfigureEvm<Primitives = <T::Types as NodeTypes>::Primitives>;

    /// Consensus engine
    type Consensus: FullConsensus<<T::Types as NodeTypes>::Primitives, Error = ConsensusError>;

    /// Network layer
    type Network: FullNetwork<Primitives: NetPrimitivesFor<<T::Types as NodeTypes>::Primitives>>;

    // Accessor methods
    fn pool(&self) -> &Self::Pool;
    fn evm_config(&self) -> &Self::Evm;
    fn consensus(&self) -> &Self::Consensus;
    fn network(&self) -> &Self::Network;
    fn payload_builder_handle(&self) -> &PayloadBuilderHandle<<T::Types as NodeTypes>::Payload>;
}
```

### 4. Node Trait - Bundling Configuration and Builders

**Location**: `crates/node/builder/src/node.rs`

Combines type configuration with preconfigured component builders:

```rust
pub trait Node<N: FullNodeTypes>: NodeTypes + Clone {
    /// Builder for node components
    type ComponentsBuilder: NodeComponentsBuilder<N>;

    /// Add-ons configuration
    type AddOns: NodeAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder;
    fn add_ons(&self) -> Self::AddOns;
}
```

This enables the convenient `.node(EthereumNode)` builder method.

## Builder State Machine

### State 1: NodeBuilder

**File**: `crates/node/builder/src/builder/mod.rs`

Initial state with only configuration:

```rust
pub struct NodeBuilder<DB, ChainSpec> {
    config: NodeConfig<ChainSpec>,
    database: DB,
}

impl<DB, ChainSpec> NodeBuilder<DB, ChainSpec> {
    pub fn new(config: NodeConfig<ChainSpec>) -> Self {
        Self { config, database: Default::default() }
    }

    pub fn with_database<D>(self, database: D) -> NodeBuilder<D, ChainSpec> {
        NodeBuilder { config: self.config, database }
    }
}
```

### State 2: WithLaunchContext

**File**: `crates/node/builder/src/builder/mod.rs`

Adds task executor:

```rust
pub struct WithLaunchContext<Builder> {
    builder: Builder,
    task_executor: TaskExecutor,
}

impl<DB, ChainSpec> WithLaunchContext<NodeBuilder<DB, ChainSpec>> {
    // Configure node types
    pub fn with_types<T>(self) -> WithLaunchContext<NodeBuilderWithTypes<...>>
    where
        T: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
    {
        // Transition to next state
    }

    // Shortcut: configure types + components at once
    pub fn node<N>(self, node: N) -> WithLaunchContext<NodeBuilderWithComponents<...>>
    where
        N: Node<...>,
    {
        self.with_types::<N>()
            .with_components(node.components_builder())
            .with_add_ons(node.add_ons())
    }
}
```

### State 3: NodeBuilderWithTypes

**File**: `crates/node/builder/src/builder/states.rs`

Types configured:

```rust
pub struct NodeBuilderWithTypes<T: FullNodeTypes> {
    config: NodeConfig<<T::Types as NodeTypes>::ChainSpec>,
    adapter: NodeTypesAdapter<T>,
}

impl<T: FullNodeTypes> NodeBuilderWithTypes<T> {
    pub fn with_components<CB>(
        self,
        components_builder: CB,
    ) -> NodeBuilderWithComponents<T, CB, ()>
    where
        CB: NodeComponentsBuilder<T>,
    {
        // Transition to components state
    }
}
```

### State 4: NodeBuilderWithComponents

**File**: `crates/node/builder/src/builder/states.rs`

Ready to launch:

```rust
pub struct NodeBuilderWithComponents<T, CB, AO>
where
    T: FullNodeTypes,
    CB: NodeComponentsBuilder<T>,
    AO: NodeAddOns<NodeAdapter<T, CB::Components>>,
{
    config: NodeConfig<<T::Types as NodeTypes>::ChainSpec>,
    adapter: NodeTypesAdapter<T>,
    components_builder: CB,
    add_ons: AddOns<NodeAdapter<T, CB::Components>, AO>,
}

impl<T, CB, AO> NodeBuilderWithComponents<T, CB, AO> {
    pub fn with_add_ons<AO2>(self, add_ons: AO2) -> NodeBuilderWithComponents<T, CB, AO2> {
        // Configure add-ons
    }

    pub async fn launch(self) -> eyre::Result<NodeHandle<...>> {
        // Build and launch node
    }
}
```

## Component Builders

### NodeComponentsBuilder Trait

**Location**: `crates/node/builder/src/components/builder.rs:432`

Builds all components together:

```rust
pub trait NodeComponentsBuilder<Node: FullNodeTypes>: Send {
    type Components: NodeComponents<Node>;

    fn build_components(
        self,
        ctx: &BuilderContext<Node>,
    ) -> impl Future<Output = eyre::Result<Self::Components>> + Send;
}
```

### ComponentsBuilder - Generic Implementation

**Location**: `crates/node/builder/src/components/builder.rs`

Provides a flexible, composable component builder:

```rust
pub struct ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB> {
    pool_builder: PoolB,
    payload_builder: PayloadB,
    network_builder: NetworkB,
    executor_builder: ExecB,
    consensus_builder: ConsB,
    _marker: PhantomData<Node>,
}

impl<Node> ComponentsBuilder<Node, (), (), (), (), ()> {
    pub fn default() -> Self {
        Self { /* ... */ }
    }

    pub fn node_types<T>(self) -> ComponentsBuilder<T, (), (), (), (), ()> {
        // Set node type
    }
}

// Builder methods enforce dependency order
impl<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
    ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
{
    // Pool is independent
    pub fn pool<PB>(self, pool_builder: PB) -> ComponentsBuilder<Node, PB, PayloadB, NetworkB, ExecB, ConsB> {
        ComponentsBuilder { pool_builder, .. }
    }

    // Payload and network depend on pool
    pub fn payload<PB>(self, payload_builder: PB) -> ComponentsBuilder<Node, PoolB, PB, NetworkB, ExecB, ConsB>
    where
        PoolB: PoolBuilder<Node>,
    {
        ComponentsBuilder { payload_builder, .. }
    }

    // Executor is independent
    pub fn executor<EB>(self, executor_builder: EB) -> ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, EB, ConsB> {
        ComponentsBuilder { executor_builder, .. }
    }

    // Consensus is independent
    pub fn consensus<CB>(self, consensus_builder: CB) -> ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, CB> {
        ComponentsBuilder { consensus_builder, .. }
    }
}
```

### Individual Component Builders

#### PoolBuilder

**Location**: `crates/node/builder/src/components/pool.rs`

```rust
pub trait PoolBuilder<Node: FullNodeTypes>: Send {
    type Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static;

    fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
    ) -> impl Future<Output = eyre::Result<Self::Pool>> + Send;
}

// Example implementation
pub struct EthereumPoolBuilder;

impl<Node> PoolBuilder<Node> for EthereumPoolBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<Primitives = EthPrimitives>>,
{
    type Pool = Pool<...>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let blob_store = ctx.blob_store()?;
        let validator = TransactionValidationTaskExecutor::eth_builder(ctx.chain_spec())
            .with_head_timestamp(ctx.head().timestamp)
            .kzg_settings(ctx.kzg_settings()?)
            .with_local_transactions_config(ctx.config().txpool.local_transactions_config.clone())
            .build_with_tasks(ctx.provider().clone(), ctx.task_executor().clone(), blob_store);

        let transaction_pool = reth_transaction_pool::Pool::eth_pool(
            validator,
            blob_store,
            ctx.pool_config(),
        );

        Ok(transaction_pool)
    }
}
```

#### ExecutorBuilder

**Location**: `crates/node/builder/src/components/execute.rs`

```rust
pub trait ExecutorBuilder<Node: FullNodeTypes>: Send {
    type EVM: ConfigureEvm<Primitives = PrimitivesTy<Node::Types>> + 'static;

    fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> impl Future<Output = eyre::Result<Self::EVM>> + Send;
}

// Example implementation
pub struct EthereumExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for EthereumExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<Primitives = EthPrimitives, ChainSpec = ChainSpec>>,
{
    type EVM = EthEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        Ok(EthEvmConfig::new(ctx.chain_spec()))
    }
}
```

#### PayloadServiceBuilder

**Location**: `crates/node/builder/src/components/payload.rs`

```rust
pub trait PayloadServiceBuilder<Node: FullNodeTypes, Pool>: Send {
    fn spawn_payload_service(
        self,
        ctx: &PayloadBuilderContext<Node, Pool>,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>>;
}

// Basic implementation
pub struct BasicPayloadServiceBuilder<Builder> {
    builder: Builder,
}

impl<Node, Pool, Builder> PayloadServiceBuilder<Node, Pool>
    for BasicPayloadServiceBuilder<Builder>
where
    Node: FullNodeTypes,
    Pool: TransactionPool + Unpin + 'static,
    Builder: PayloadBuilder<BuiltPayload: BuiltPayload<Primitives = Node::Primitives>> + 'static,
{
    fn spawn_payload_service(
        self,
        ctx: &PayloadBuilderContext<Node, Pool>,
    ) -> eyre::Result<PayloadBuilderHandle<...>> {
        let payload_job_generator = BasicPayloadJobGenerator::new(
            ctx.provider().clone(),
            ctx.pool().clone(),
            ctx.task_executor().clone(),
            Default::default(),
            ctx.chain_spec(),
            self.builder,
        );

        let (payload_service, payload_builder) = PayloadBuilderService::new(
            payload_job_generator,
            ctx.provider().canonical_state_stream(),
        );

        ctx.task_executor().spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}
```

### BuilderContext

**Location**: `crates/node/builder/src/components/builder.rs`

Provides access to configuration and shared resources:

```rust
pub struct BuilderContext<Node: FullNodeTypes> {
    head: Head,
    provider: Node::Provider,
    executor: TaskExecutor,
    config: &NodeConfig<<Node::Types as NodeTypes>::ChainSpec>,
    // ... other fields
}

impl<Node: FullNodeTypes> BuilderContext<Node> {
    pub fn head(&self) -> Head { self.head }
    pub fn provider(&self) -> &Node::Provider { &self.provider }
    pub fn task_executor(&self) -> &TaskExecutor { &self.executor }
    pub fn config(&self) -> &NodeConfig<...> { self.config }
    pub fn chain_spec(&self) -> Arc<<Node::Types as NodeTypes>::ChainSpec> { ... }
    pub fn pool_config(&self) -> PoolConfig { ... }
    // ... more accessors
}
```

## Complete Examples

### Example 1: Ethereum Node

**Location**: `crates/ethereum/node/src/node.rs`

```rust
#[derive(Debug, Default, Clone, Copy)]
pub struct EthereumNode;

impl NodeTypes for EthereumNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ChainSpec;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

impl<N> Node<N> for EthereumNode
where
    N: FullNodeTypes<Types: NodeTypes<Primitives = EthPrimitives, ChainSpec = ChainSpec>>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        BasicPayloadServiceBuilder<EthereumPayloadBuilder>,
        EthereumNetworkBuilder,
        EthereumExecutorBuilder,
        EthereumConsensusBuilder,
    >;

    type AddOns = EthereumAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        EthApi<...>,
        EthEngineValidatorBuilder,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(EthereumPoolBuilder::default())
            .executor(EthereumExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::default())
            .network(EthereumNetworkBuilder::default())
            .consensus(EthereumConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        EthereumAddOns::default()
    }
}
```

**Usage**:

```rust
let handle = NodeBuilder::new(config)
    .with_database(db)
    .with_launch_context(task_executor)
    .node(EthereumNode::default())
    .launch()
    .await?;
```

### Example 2: Optimism Node

**Location**: `crates/optimism/node/src/node.rs`

```rust
#[derive(Debug, Default, Clone)]
pub struct OpNode {
    pub args: RollupArgs,
    pub da_config: OpDAConfig,
    pub gas_limit_config: OpGasLimitConfig,
}

impl NodeTypes for OpNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

impl<N> Node<N> for OpNode
where
    N: FullNodeTypes<Types: OpFullNodeTypes + OpNodeTypes>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        OpPoolBuilder,
        BasicPayloadServiceBuilder<OpPayloadBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns = OpAddOns<...>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(OpPoolBuilder::default())
            .executor(OpExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(
                OpPayloadBuilder::new(
                    self.da_config.clone(),
                    self.gas_limit_config.clone(),
                )
            ))
            .network(OpNetworkBuilder::new(
                self.args.disable_txpool_gossip,
                self.args.disable_discovery_v4,
            ))
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        OpAddOns::builder()
            .with_sequencer(self.args.sequencer_http.clone())
            .build()
    }
}
```

### Example 3: Custom Node with Mixed Components

```rust
#[derive(Debug, Clone)]
pub struct HybridNode {
    pub custom_config: MyConfig,
}

impl NodeTypes for HybridNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ChainSpec;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

impl<N> Node<N> for HybridNode
where
    N: FullNodeTypes<Types: NodeTypes<Primitives = EthPrimitives>>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        MyCustomPoolBuilder,              // Custom pool
        BasicPayloadServiceBuilder<EthereumPayloadBuilder>,  // Standard payload
        EthereumNetworkBuilder,            // Standard network
        EthereumExecutorBuilder,           // Standard executor
        MyCustomConsensusBuilder,          // Custom consensus
    >;

    type AddOns = EthereumAddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(MyCustomPoolBuilder::new(self.custom_config.clone()))
            .executor(EthereumExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::default())
            .network(EthereumNetworkBuilder::default())
            .consensus(MyCustomConsensusBuilder::new(self.custom_config.clone()))
    }

    fn add_ons(&self) -> Self::AddOns {
        EthereumAddOns::default()
    }
}
```

## Customization Patterns

### Pattern 1: Wrap Standard Component

```rust
pub struct MyCustomPool {
    inner: Pool<...>,
    custom_state: MyState,
}

pub struct MyCustomPoolBuilder {
    config: MyConfig,
}

impl<Node> PoolBuilder<Node> for MyCustomPoolBuilder {
    type Pool = MyCustomPool;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        // Build standard pool
        let inner = EthereumPoolBuilder::default().build_pool(ctx).await?;

        // Wrap with custom logic
        Ok(MyCustomPool {
            inner,
            custom_state: MyState::new(self.config),
        })
    }
}
```

### Pattern 2: Replace Component Entirely

```rust
pub struct MyPoolBuilder;

impl<Node> PoolBuilder<Node> for MyPoolBuilder {
    type Pool = MyCompletelyCustomPool;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        // Build entirely custom pool
        Ok(MyCompletelyCustomPool::new(
            ctx.provider().clone(),
            ctx.chain_spec(),
        ))
    }
}
```

### Pattern 3: Conditional Components

```rust
pub struct ConditionalPoolBuilder {
    use_custom: bool,
}

impl<Node> PoolBuilder<Node> for ConditionalPoolBuilder {
    type Pool = Pool<...>;  // Same type for both

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        if self.use_custom {
            // Build custom variant
            build_custom_pool(ctx).await
        } else {
            // Build standard variant
            EthereumPoolBuilder::default().build_pool(ctx).await
        }
    }
}
```

## Node Add-Ons

**Location**: `crates/node/api/src/node.rs`

Add-ons are launched after core components are ready:

```rust
pub trait NodeAddOns<N: FullNodeComponents>: Send {
    type Handle: Send + Sync + Clone;

    async fn launch_add_ons(
        self,
        ctx: AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle>;
}

pub struct AddOnsContext<'a, N: FullNodeComponents> {
    pub node: N,
    pub config: &'a NodeConfig<<N::Types as NodeTypes>::ChainSpec>,
    pub beacon_engine_handle: ConsensusEngineHandle<<N::Types as NodeTypes>::Payload>,
    pub engine_events: EventSender<ConsensusEngineEvent<...>>,
    pub jwt_secret: JwtSecret,
}
```

**Example - EthereumAddOns**:

```rust
impl<N, EthApi, EngineValidator> NodeAddOns<N> for EthereumAddOns<N, EthApi, EngineValidator>
where
    N: FullNodeComponents<Types: NodeTypes<Primitives = EthPrimitives>>,
    EthApi: EthApiBuilder<N>,
    EngineValidator: EngineValidatorBuilder<N>,
{
    type Handle = AddOnsHandle;

    async fn launch_add_ons(self, ctx: AddOnsContext<'_, N>) -> eyre::Result<Self::Handle> {
        // Launch RPC server
        let rpc_server = RpcServer::launch(...).await?;

        // Launch engine validator
        let engine_validator = self.engine_validator_builder.build(...);

        Ok(AddOnsHandle {
            rpc: rpc_server,
            engine_validator,
        })
    }
}
```

## Key Takeaways

1. **Type Safety**: The builder enforces correct configuration order at compile time
2. **Modularity**: Each component can be customized independently
3. **Composition**: Mix standard and custom components freely
4. **Flexibility**: Use closure-based builders for simple cases, traits for complex ones
5. **Reusability**: Share component builders across different node types

## Next Steps

- Read the [Engine & Consensus Guide](./ENGINE_CONSENSUS_GUIDE.md) to customize validation
- Read the [Payload Builder Guide](./PAYLOAD_BUILDER_GUIDE.md) to customize block building
- Study the Optimism implementation for real-world patterns
- Experiment with replacing individual components
