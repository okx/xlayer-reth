# ChainSpec & EVM Configuration Guide

This guide explains how to create custom chain specifications and configure EVM behavior for custom chains, L2s, or private networks.

## Table of Contents

1. [Overview](#overview)
2. [Chain Specification](#chain-specification)
3. [Hardfork Management](#hardfork-management)
4. [EVM Configuration](#evm-configuration)
5. [Custom Precompiles](#custom-precompiles)
6. [Complete Examples](#complete-examples)

## Overview

Reth's chain and EVM configuration is split into two main concerns:

- **ChainSpec**: Defines chain parameters, genesis, and hardfork schedule
- **EvmConfig**: Configures EVM execution behavior, precompiles, and gas schedules

This separation allows you to customize chain rules independently from execution logic.

### Key Components

```
ChainSpec
  ├─ Chain ID
  ├─ Genesis state
  ├─ Hardfork schedule
  ├─ Base fee parameters
  └─ Blob parameters

EvmConfig
  ├─ Block executor factory
  ├─ Block assembler
  ├─ EVM environment setup
  └─ Precompile configuration
```

## Chain Specification

### ChainSpec Structure

**Location**: `crates/chainspec/src/spec.rs`

```rust
pub struct ChainSpec<H: BlockHeader = Header> {
    /// The chain ID (e.g., 1 for mainnet)
    pub chain: Chain,

    /// Genesis block configuration
    pub genesis: Genesis,

    /// Genesis header
    pub genesis_header: SealedHeader<H>,

    /// Paris hardfork block and final difficulty (for PoS transition)
    pub paris_block_and_final_difficulty: Option<(u64, U256)>,

    /// Hardfork activation schedule
    pub hardforks: ChainHardforks,

    /// Deposit contract for PoS (optional)
    pub deposit_contract: Option<DepositContract>,

    /// Base fee parameters (constant or variable per hardfork)
    pub base_fee_params: BaseFeeParamsKind,

    /// Pruner delete limit per run
    pub prune_delete_limit: usize,

    /// Blob gas parameters schedule
    pub blob_params: BlobScheduleBlobParams,
}
```

### EthChainSpec Trait

**Location**: `crates/chainspec/src/api.rs`

Core trait for chain specifications:

```rust
pub trait EthChainSpec: Send + Sync + Unpin + Debug {
    type Header: BlockHeader;

    /// Chain identifier
    fn chain(&self) -> Chain;

    /// Chain ID for transaction signing
    fn chain_id(&self) -> u64 {
        self.chain().id()
    }

    /// Get base fee parameters at timestamp
    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams;

    /// Get blob parameters at timestamp
    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams>;

    /// Deposit contract address
    fn deposit_contract(&self) -> Option<&DepositContract>;

    /// Genesis block hash
    fn genesis_hash(&self) -> B256;

    /// Genesis header
    fn genesis_header(&self) -> &Self::Header;

    /// Genesis state
    fn genesis(&self) -> &Genesis;

    /// Boot nodes
    fn bootnodes(&self) -> Option<Vec<NodeRecord>>;

    /// Is this Optimism?
    fn is_optimism(&self) -> bool {
        false
    }

    /// Is this Ethereum?
    fn is_ethereum(&self) -> bool {
        !self.is_optimism()
    }

    /// Final total difficulty for Paris (PoS transition)
    fn final_paris_total_difficulty(&self) -> Option<U256>;

    /// Calculate next block's base fee
    fn next_block_base_fee(
        &self,
        parent: &Self::Header,
        target_timestamp: u64,
    ) -> Option<u64>;
}
```

### Creating a ChainSpec

#### Using the Builder

```rust
use reth_chainspec::{ChainSpec, Chain, Genesis};

let spec = ChainSpec::builder()
    .chain(Chain::from_id(12345))
    .genesis(Genesis::default())
    .london_activated()
    .paris_activated()
    .shanghai_activated()
    .cancun_activated()
    .with_fork(EthereumHardfork::Prague, ForkCondition::Timestamp(1700000000))
    .build();
```

#### From Genesis File

```rust
use reth_chainspec::ChainSpec;

let genesis_json = std::fs::read_to_string("genesis.json")?;
let spec = ChainSpec::from_json(&genesis_json)?;
```

#### Genesis JSON Format

```json
{
  "config": {
    "chainId": 12345,
    "homesteadBlock": 0,
    "eip150Block": 0,
    "eip155Block": 0,
    "eip158Block": 0,
    "byzantiumBlock": 0,
    "constantinopleBlock": 0,
    "petersburgBlock": 0,
    "istanbulBlock": 0,
    "berlinBlock": 0,
    "londonBlock": 0,
    "shanghaiTime": 1681338455,
    "cancunTime": 1705473120
  },
  "nonce": "0x0",
  "timestamp": "0x0",
  "extraData": "0x",
  "gasLimit": "0x1c9c380",
  "difficulty": "0x1",
  "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "coinbase": "0x0000000000000000000000000000000000000000",
  "alloc": {
    "0x1234567890abcdef1234567890abcdef12345678": {
      "balance": "0x200000000000000000000000000000000000000000000000000000000000000"
    }
  }
}
```

## Hardfork Management

### Hardforks Trait

**Location**: `crates/ethereum/hardforks/src/hardforks/mod.rs`

```rust
pub trait Hardforks: Clone {
    /// Get fork activation condition
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition;

    /// Iterate all hardforks
    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)>;

    /// Is fork active at timestamp?
    fn is_fork_active_at_timestamp<H: Hardfork>(&self, fork: H, timestamp: u64) -> bool;

    /// Is fork active at block?
    fn is_fork_active_at_block<H: Hardfork>(&self, fork: H, block_number: u64) -> bool;

    /// Get fork ID for given head
    fn fork_id(&self, head: &Head) -> ForkId;

    /// Get latest fork ID
    fn latest_fork_id(&self) -> ForkId;

    /// Get fork filter for devp2p
    fn fork_filter(&self, head: Head) -> ForkFilter;
}
```

### ForkCondition

Activation conditions for hardforks:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkCondition {
    /// Never activated
    Never,

    /// Activated at specific block number
    Block(u64),

    /// Activated at specific timestamp
    Timestamp(u64),

    /// Activated by total difficulty (for Paris/PoS transition)
    TTD {
        fork_block: Option<u64>,
        total_difficulty: U256,
    },
}
```

### ChainHardforks

**Location**: `crates/ethereum/hardforks/src/hardforks/mod.rs`

Manages hardfork schedule:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainHardforks {
    forks: Vec<(Box<dyn Hardfork>, ForkCondition)>,
    map: FxHashMap<&'static str, ForkCondition>,
}

impl ChainHardforks {
    /// Create from list of forks (must be in chronological order)
    pub fn new(forks: Vec<(Box<dyn Hardfork>, ForkCondition)>) -> Self {
        let map = forks.iter()
            .map(|(fork, condition)| (fork.name(), condition.clone()))
            .collect();

        Self { forks, map }
    }

    /// Get fork condition
    pub fn fork(&self, fork: impl Hardfork) -> ForkCondition {
        self.map.get(fork.name())
            .cloned()
            .unwrap_or(ForkCondition::Never)
    }

    /// Insert fork maintaining chronological order
    pub fn insert(&mut self, fork: impl Hardfork + 'static, condition: ForkCondition) {
        // Remove if exists
        self.forks.retain(|(f, _)| f.name() != fork.name());

        // Insert in correct position
        let boxed_fork: Box<dyn Hardfork> = Box::new(fork);
        self.forks.push((boxed_fork.clone(), condition.clone()));

        // Re-sort
        self.sort();

        // Update map
        self.map.insert(boxed_fork.name(), condition);
    }
}
```

### Standard Ethereum Hardforks

**Location**: `crates/ethereum/hardforks/src/hardforks/ethereum.rs`

```rust
#[derive(Debug, Clone, Copy)]
pub enum EthereumHardfork {
    Frontier,
    Homestead,
    Dao,
    Tangerine,
    SpuriousDragon,
    Byzantium,
    Constantinople,
    Petersburg,
    Istanbul,
    MuirGlacier,
    Berlin,
    London,
    ArrowGlacier,
    GrayGlacier,
    Paris,      // The Merge
    Shanghai,
    Cancun,
    Prague,
    Osaka,
}
```

### Custom Hardforks

**Location**: `examples/custom-hardforks/src/chainspec.rs`

Define custom hardforks using the `hardfork!` macro:

```rust
use reth_chainspec::hardfork;

// Define custom hardforks
hardfork!(
    MyHardforks {
        BasicUpgrade,
        AdvancedUpgrade,
        FutureUpgrade,
    }
);

// Custom chain spec
#[derive(Debug, Clone)]
pub struct MyChainSpec {
    pub inner: ChainSpec,
}

impl MyChainSpec {
    pub fn from_genesis(genesis: Genesis) -> Self {
        // Extract custom hardfork config from genesis extra fields
        let extra = genesis.config.extra_fields
            .deserialize_as::<MyHardforkConfig>()
            .unwrap();

        let mut inner = ChainSpec::from_genesis(genesis);

        // Add custom hardforks
        inner.hardforks.insert(
            MyHardforks::BasicUpgrade,
            ForkCondition::Timestamp(extra.basic_upgrade_time),
        );
        inner.hardforks.insert(
            MyHardforks::AdvancedUpgrade,
            ForkCondition::Timestamp(extra.advanced_upgrade_time),
        );

        Self { inner }
    }

    /// Check if basic upgrade is active
    pub fn is_basic_upgrade_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.inner.hardforks.is_fork_active_at_timestamp(
            MyHardforks::BasicUpgrade,
            timestamp,
        )
    }
}

// Implement required traits
impl Hardforks for MyChainSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.inner.forks_iter()
    }

    // ... other methods delegate to inner
}

impl EthChainSpec for MyChainSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        self.inner.chain()
    }

    // ... other methods delegate to inner
}
```

## EVM Configuration

### ConfigureEvm Trait

**Location**: `crates/evm/evm/src/lib.rs`

Core trait for EVM configuration:

```rust
pub trait ConfigureEvm: Clone + Debug + Send + Sync + Unpin {
    /// Primitive types used by EVM
    type Primitives: NodePrimitives;

    /// Error type
    type Error: Error + Send + Sync + 'static;

    /// Context for next block environment
    type NextBlockEnvCtx: Debug + Clone;

    /// Block executor factory
    type BlockExecutorFactory: BlockExecutorFactory<...>;

    /// Block assembler
    type BlockAssembler: BlockAssembler<Self::BlockExecutorFactory, ...>;

    /// Get executor factory
    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory;

    /// Get block assembler
    fn block_assembler(&self) -> &Self::BlockAssembler;

    /// Create EVM environment for existing block
    fn evm_env(
        &self,
        header: &HeaderTy<Self::Primitives>,
    ) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// Create EVM environment for next block
    fn next_evm_env(
        &self,
        parent: &HeaderTy<Self::Primitives>,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error>;

    /// Create execution context for block
    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<BlockTy<Self::Primitives>>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error>;

    /// Create execution context for next block
    fn context_for_next_block(
        &self,
        parent: &SealedHeader<HeaderTy<Self::Primitives>>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error>;

    /// Create block builder
    fn builder_for_next_block<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
        parent: &'a SealedHeader<...>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<impl BlockBuilder<...>, Self::Error>;

    /// Create executor
    fn executor<DB: Database>(
        &self,
        db: DB,
    ) -> impl Executor<DB, Primitives = Self::Primitives>;
}
```

### Ethereum EVM Config

**Location**: `crates/ethereum/evm/src/lib.rs`

```rust
#[derive(Debug, Clone)]
pub struct EthEvmConfig<C = ChainSpec, EvmFactory = EthEvmFactory> {
    pub executor_factory: EthBlockExecutorFactory<RethReceiptBuilder, Arc<C>, EvmFactory>,
    pub block_assembler: EthBlockAssembler<C>,
}

impl<ChainSpec, EvmFactory> EthEvmConfig<ChainSpec, EvmFactory> {
    /// Create with default EVM factory
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self
    where
        ChainSpec: EthChainSpec + EthereumHardforks,
        EvmFactory: Default,
    {
        Self::new_with_evm_factory(chain_spec, EvmFactory::default())
    }

    /// Create with custom EVM factory
    pub fn new_with_evm_factory(
        chain_spec: Arc<ChainSpec>,
        evm_factory: EvmFactory,
    ) -> Self {
        Self {
            block_assembler: EthBlockAssembler::new(chain_spec.clone()),
            executor_factory: EthBlockExecutorFactory::new(
                RethReceiptBuilder::default(),
                chain_spec,
                evm_factory,
            ),
        }
    }
}

impl<ChainSpec, EvmF> ConfigureEvm for EthEvmConfig<ChainSpec, EvmF>
where
    ChainSpec: EthExecutorSpec + EthChainSpec<Header = Header> + Hardforks + 'static,
    EvmF: EvmFactory<...> + Clone + Debug + Send + Sync + Unpin + 'static,
{
    type Primitives = EthPrimitives;
    type Error = Infallible;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, EvmF>;
    type BlockAssembler = EthBlockAssembler<ChainSpec>;

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        Ok(EvmEnv::for_eth_block(
            header,
            self.chain_spec(),
            self.chain_spec().chain().id(),
            self.chain_spec().blob_params_at_timestamp(header.timestamp),
        ))
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        Ok(EvmEnv::for_eth_next_block(
            parent,
            NextEvmEnvAttributes {
                timestamp: attributes.timestamp,
                suggested_fee_recipient: attributes.suggested_fee_recipient,
                prev_randao: attributes.prev_randao,
                gas_limit: attributes.gas_limit,
                withdrawals: attributes.withdrawals.clone(),
                parent_beacon_block_root: attributes.parent_beacon_block_root,
            },
            self.chain_spec().next_block_base_fee(parent, attributes.timestamp)?,
            self.chain_spec(),
            self.chain_spec().chain().id(),
            self.chain_spec().blob_params_at_timestamp(attributes.timestamp),
        ))
    }
}
```

### Block Assembler

**Location**: `crates/ethereum/evm/src/build.rs`

Constructs final blocks after execution:

```rust
#[derive(Debug, Clone)]
pub struct EthBlockAssembler<ChainSpec = reth_chainspec::ChainSpec> {
    pub chain_spec: Arc<ChainSpec>,
    pub extra_data: Bytes,
}

impl<F, ChainSpec> BlockAssembler<F> for EthBlockAssembler<ChainSpec>
where
    F: BlockExecutorFactory<...>,
    ChainSpec: EthChainSpec + EthereumHardforks,
{
    type Block = Block<F::Transaction>;

    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, F>,
    ) -> Result<Self::Block, BlockExecutionError> {
        let BlockAssemblerInput {
            parent,
            evm_env,
            transactions,
            output,
            versioned_hashes,
        } = input;

        // Calculate roots
        let receipts_root = output.receipts_root_slow(...);
        let transactions_root = output.transactions_root_slow(...);

        // Calculate withdrawals root (Shanghai+)
        let withdrawals_root = if self.chain_spec
            .is_shanghai_active_at_timestamp(evm_env.block_env.timestamp)
        {
            Some(evm_env.block_env.withdrawals.root_slow())
        } else {
            None
        };

        // Calculate requests hash (Prague+)
        let requests_hash = if self.chain_spec
            .is_prague_active_at_timestamp(evm_env.block_env.timestamp)
        {
            Some(output.requests.hash_slow())
        } else {
            None
        };

        // Blob gas calculations (Cancun+)
        let (blob_gas_used, excess_blob_gas) = if self.chain_spec
            .is_cancun_active_at_timestamp(evm_env.block_env.timestamp)
        {
            let blob_gas_used = calculate_blob_gas_used(versioned_hashes);
            let excess_blob_gas = calculate_excess_blob_gas(
                parent.excess_blob_gas.unwrap_or_default(),
                parent.blob_gas_used.unwrap_or_default(),
            );
            (Some(blob_gas_used), Some(excess_blob_gas))
        } else {
            (None, None)
        };

        // Build header
        let header = Header {
            parent_hash: parent.hash(),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: evm_env.block_env.coinbase,
            state_root: output.state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom: output.logs_bloom,
            difficulty: U256::ZERO,
            number: parent.number + 1,
            gas_limit: evm_env.block_env.gas_limit as u64,
            gas_used: output.gas_used,
            timestamp: evm_env.block_env.timestamp,
            mix_hash: evm_env.block_env.prevrandao,
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: evm_env.block_env.basefee,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_block_root: evm_env.block_env.parent_beacon_block_root,
            requests_hash,
            extra_data: self.extra_data.clone(),
        };

        Ok(Block {
            header,
            body: BlockBody {
                transactions: transactions.to_vec(),
                ommers: vec![],
                withdrawals: evm_env.block_env.withdrawals,
            },
        })
    }
}
```

## Custom Precompiles

### EvmFactory Trait

**Location**: `crates/evm/src/factory.rs`

```rust
pub trait EvmFactory: Clone {
    type Evm<DB: Database, I: Inspector<...>>: Evm<DB, I>;
    type Tx: TxEnv;
    type Spec: Spec;
    type BlockEnv: BlockEnv;
    type Precompiles: Precompiles;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv,
    ) -> Self::Evm<DB, NoOpInspector>;
}
```

### Example: Custom Precompile

**Location**: `examples/custom-evm/src/main.rs`

```rust
use revm::precompile::{Precompile, PrecompileId, PrecompileResult, Precompiles};

#[derive(Debug, Clone, Default)]
pub struct MyEvmFactory;

impl EvmFactory for MyEvmFactory {
    type Evm<DB: Database, I: Inspector<...>> = EthEvm<DB, I, Self::Precompiles>;
    type Tx = TxEnv;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv,
    ) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;

        let mut evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(PrecompilesMap::from_static(
                EthPrecompiles::default().precompiles
            ));

        // Add custom precompiles for Prague+
        if spec == SpecId::PRAGUE {
            evm = evm.with_precompiles(PrecompilesMap::from_static(
                prague_with_custom_precompiles()
            ));
        }

        EthEvm::new(evm, false)
    }
}

/// Get Prague precompiles with custom additions
fn prague_with_custom_precompiles() -> &'static Precompiles {
    static INSTANCE: OnceLock<Precompiles> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let mut precompiles = Precompiles::prague().clone();

        // Add custom precompile at address 0x999
        let custom_precompile = Precompile::new(
            PrecompileId::custom("my_custom_precompile"),
            address!("0x0000000000000000000000000000000000000999"),
            |input, gas_limit| {
                // Custom precompile logic
                // input: call data
                // gas_limit: available gas

                // Example: reverse input bytes
                let output = input.iter().rev().copied().collect::<Bytes>();

                PrecompileResult::Ok(PrecompileOutput {
                    gas_used: 100,  // Gas cost
                    bytes: output,
                })
            },
        );

        precompiles.extend([custom_precompile]);
        precompiles
    })
}
```

### Using Custom EVM Factory

```rust
use reth::builder::NodeBuilder;

// In node builder
let components = ComponentsBuilder::default()
    .node_types::<MyNode>()
    .pool(EthereumPoolBuilder::default())
    .executor(MyCustomExecutorBuilder)  // Uses custom EVM factory
    .payload(BasicPayloadServiceBuilder::default())
    .network(EthereumNetworkBuilder::default())
    .consensus(EthereumConsensusBuilder::default());

// Custom executor builder
#[derive(Debug, Default, Clone)]
pub struct MyCustomExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for MyCustomExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = EthEvmConfig<ChainSpec, MyEvmFactory>;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<Self::EVM> {
        let evm_config = EthEvmConfig::new_with_evm_factory(
            ctx.chain_spec(),
            MyEvmFactory::default(),
        );
        Ok(evm_config)
    }
}
```

## Complete Examples

### Example 1: Custom L2 Chain

```rust
use reth_chainspec::{ChainSpec, Chain, hardfork};

// Define L2-specific hardforks
hardfork!(
    L2Hardforks {
        L2Genesis,
        L2Upgrade1,
        L2Upgrade2,
    }
);

#[derive(Debug, Clone)]
pub struct L2ChainSpec {
    pub inner: ChainSpec,
    pub l1_bridge_address: Address,
}

impl L2ChainSpec {
    pub fn new(
        chain_id: u64,
        l1_bridge_address: Address,
        l2_upgrade1_time: u64,
        l2_upgrade2_time: u64,
    ) -> Self {
        let mut inner = ChainSpec::builder()
            .chain(Chain::from_id(chain_id))
            .genesis(Genesis::default())
            .paris_activated()  // PoS from start
            .shanghai_activated()
            .cancun_activated()
            .build();

        // Add L2 hardforks
        inner.hardforks.insert(
            L2Hardforks::L2Genesis,
            ForkCondition::Timestamp(0),
        );
        inner.hardforks.insert(
            L2Hardforks::L2Upgrade1,
            ForkCondition::Timestamp(l2_upgrade1_time),
        );
        inner.hardforks.insert(
            L2Hardforks::L2Upgrade2,
            ForkCondition::Timestamp(l2_upgrade2_time),
        );

        Self { inner, l1_bridge_address }
    }

    pub fn is_l2_upgrade1_active(&self, timestamp: u64) -> bool {
        self.inner.hardforks.is_fork_active_at_timestamp(
            L2Hardforks::L2Upgrade1,
            timestamp,
        )
    }
}

impl EthChainSpec for L2ChainSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        self.inner.chain()
    }

    fn is_optimism(&self) -> bool {
        true  // Mark as L2
    }

    // Delegate other methods to inner
    fn chain_id(&self) -> u64 { self.inner.chain_id() }
    fn genesis_hash(&self) -> B256 { self.inner.genesis_hash() }
    // ... etc
}
```

### Example 2: Custom Base Fee Calculation

**Location**: `crates/optimism/chainspec/src/lib.rs`

Optimism uses variable base fee parameters:

```rust
impl EthChainSpec for OpChainSpec {
    fn next_block_base_fee(
        &self,
        parent: &Header,
        target_timestamp: u64,
    ) -> Option<u64> {
        // Jovian: Custom base fee formula
        if self.is_jovian_active_at_timestamp(parent.timestamp()) {
            return compute_jovian_base_fee(self, parent, target_timestamp).ok();
        }

        // Holocene: Decode from parent extra data
        if self.is_holocene_active_at_timestamp(parent.timestamp()) {
            return decode_holocene_base_fee(self, parent, target_timestamp).ok();
        }

        // Default Ethereum calculation
        self.inner.next_block_base_fee(parent, target_timestamp)
    }
}
```

### Example 3: Blob Parameters Schedule

**Location**: `crates/chainspec/src/spec.rs`

```rust
pub static MAINNET: LazyLock<Arc<ChainSpec>> = LazyLock::new(|| {
    let mut spec = ChainSpec {
        // ... other config
        blob_params: BlobScheduleBlobParams::default()
            .with_scheduled([
                // Cancun: initial blob params
                (mainnet::MAINNET_CANCUN_TIMESTAMP, BlobParams::cancun()),
                // BPO1: first blob parameter optimization
                (mainnet::MAINNET_BPO1_TIMESTAMP, BlobParams::bpo1()),
                // BPO2: second optimization
                (mainnet::MAINNET_BPO2_TIMESTAMP, BlobParams::bpo2()),
            ]),
    };
    Arc::new(spec)
});
```

## Key Takeaways

1. **Separation of Concerns**: ChainSpec handles chain rules, EvmConfig handles execution
2. **Hardfork Flexibility**: Support block-based, timestamp-based, and TTD activation
3. **Variable Parameters**: Base fee and blob params can change per hardfork
4. **Custom Precompiles**: Add chain-specific precompiles via EvmFactory
5. **Type Safety**: Strong typing prevents configuration mismatches

## Next Steps

- Read the [Node Builder Guide](./NODE_BUILDER_GUIDE.md) to integrate custom chains
- Read the [Payload Builder Guide](./PAYLOAD_BUILDER_GUIDE.md) for custom block building
- Study the Optimism chain spec for L2 patterns
- Experiment with custom hardforks and precompiles
- Test thoroughly with different hardfork combinations
