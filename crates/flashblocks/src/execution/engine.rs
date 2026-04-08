use crate::{
    execution::{FlashblockSequenceValidator, OverlayProviderFactory},
    BuildArgs, FlashblockReceipt, FlashblockStateCache,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use alloy_consensus::Block;
use alloy_eips::{eip2718::Encodable2718, BlockNumHash};
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::ExecutedBlock;
use reth_engine_primitives::{ExecutionPayload, PayloadValidator};
use reth_engine_tree::tree::{
    payload_validator::{TreeCtx, ValidationOutcome},
    BasicEngineValidator, EngineValidator, PayloadProcessor,
};
use reth_evm::{ConfigureEngineEvm, ConfigureEvm};
use reth_node_api::{
    AddOnsContext, BlockTy, FullNodeComponents, NodeTypes, PayloadTypes, ReceiptTy, TxTy,
};
use reth_node_builder::rpc::{
    BasicEngineValidatorBuilder, EngineValidatorBuilder, PayloadValidatorBuilder,
};
use reth_optimism_forks::OpHardforks;
use reth_payload_primitives::{BuiltPayload, InvalidPayloadAttributesError, NewPayloadError};
use reth_primitives_traits::{NodePrimitives, Recovered, SealedBlock, WithEncoded};
use reth_provider::{
    BlockReader, CanonStateSubscriptions, HashedPostStateProvider, HeaderProvider,
    StateProviderFactory, StateReader,
};
use reth_trie_db::ChangesetCache;

/// X Layer engine validator — is a unified controller holding both the underlying
/// engine validator and the flashblocks sequence validator.
/// 1. Enables sharing of the underlying payload processor and the validation
///    caches (execution cache, trie changeset cache and the sparse trie) between
///    flashblocks validation and engine validation.
/// 2. Guards against payload validation races from two incoming sources, the
///    default CL/EL sync and incoming stream of flashblocks.
/// 3. Prevent double computation during races between the two sources and ensure
///    atomicity of every payload validation operation. For example, if the
///    flashblocks sequence validator is in the process of validating a payload and
///    the engine validator receives the same payload, the unified X Layer engine
///    validator will guard against this.
#[derive(Clone, Debug)]
#[expect(clippy::type_complexity)]
pub struct XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    inner: Arc<Mutex<XLayerEngineValidatorInner<Provider, Evm, V, N, ChainSpec>>>,
    validator: V,
}

impl<Provider, Evm, V, N, ChainSpec> XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm<Primitives = N>,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    /// Creates a new `XLayerEngineValidator` with only the engine validator.
    fn new(engine_validator: BasicEngineValidator<Provider, Evm, V>, validator: V) -> Self {
        Self {
            inner: Arc::new(Mutex::new(XLayerEngineValidatorInner {
                engine_validator,
                fb_validator: None,
                fb_state: None,
            })),
            validator,
        }
    }

    /// Flashblocks init, sets the flashblocks validator and state cache.
    pub fn set_flashblocks(
        &self,
        fb_validator: FlashblockSequenceValidator<N, Evm, Provider, ChainSpec>,
        fb_state: FlashblockStateCache<N>,
    ) {
        tokio::task::block_in_place(|| {
            let mut guard = self.inner.blocking_lock();
            guard.fb_state = Some(fb_state);
            guard.fb_validator = Some(fb_validator);
        });
    }

    /// Flashblocks init, gets the engine validator's changeset cache.
    pub fn get_changeset_cache(&self) -> ChangesetCache {
        tokio::task::block_in_place(|| {
            self.inner.blocking_lock().engine_validator.changeset_cache()
        })
    }

    /// Flashblocks init, gets the engine validator's shared payload processor.
    pub fn get_payload_processor(&self) -> PayloadProcessor<Evm> {
        tokio::task::block_in_place(|| {
            self.inner.blocking_lock().engine_validator.payload_processor()
        })
    }
}

impl<Provider, Evm, V, N, ChainSpec, Types> EngineValidator<Types>
    for XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>
where
    BasicEngineValidator<Provider, Evm, V>: EngineValidator<Types, N>,
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718,
    N::Block: From<Block<N::SignedTx>>,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send>
        + Send
        + 'static,
    Provider: StateProviderFactory
        + HeaderProvider<Header = <N as NodePrimitives>::BlockHeader>
        + OverlayProviderFactory
        + BlockReader
        + StateReader
        + HashedPostStateProvider
        + Unpin
        + Clone
        + Send
        + 'static,
    ChainSpec: OpHardforks + Send + Sync + 'static,
    Types: PayloadTypes<BuiltPayload: BuiltPayload<Primitives = N>>,
    Types::ExecutionData: ExecutionPayload,
    V: PayloadValidator<Types, Block = N::Block> + Send + Sync + 'static,
{
    fn validate_payload_attributes_against_header(
        &self,
        attr: &Types::PayloadAttributes,
        header: &N::BlockHeader,
    ) -> Result<(), InvalidPayloadAttributesError> {
        self.validator.validate_payload_attributes_against_header(attr, header)
    }

    fn convert_payload_to_block(
        &self,
        payload: Types::ExecutionData,
    ) -> Result<SealedBlock<N::Block>, NewPayloadError> {
        let block = self.validator.convert_payload_to_block(payload)?;
        Ok(block)
    }

    fn validate_payload(
        &mut self,
        payload: Types::ExecutionData,
        ctx: TreeCtx<'_, N>,
    ) -> ValidationOutcome<N> {
        let num_hash = payload.num_hash();
        // SAFETY: `blocking_lock()` is safe here because the engine tree runs on a dedicated
        // OS thread (`spawn_os_thread("engine", ...)` in `reth-engine-tree`), not a tokio
        // worker. These `EngineValidator` trait methods are only called from
        // `insert_block_or_payload` within that thread's synchronous `run()` loop.
        // The init helpers (`set_flashblocks`, `get_changeset_cache`, `get_payload_processor`)
        // use `block_in_place` instead because they are called from async context during
        // node startup.
        let mut guard = self.inner.blocking_lock();

        if let Some(executed_block) = guard.try_flashblocks_cache_hit(&num_hash) {
            return Ok(executed_block);
        }

        debug!(
            target: "flashblocks::engine_validator",
            block_number = num_hash.number,
            block_hash = %num_hash.hash,
            "Flashblocks cache miss, engine validating payload",
        );
        let executed_block = guard.engine_validator.validate_payload(payload, ctx)?;
        guard.try_advance_flashblocks_state(&executed_block);
        Ok(executed_block)
    }

    fn validate_block(
        &mut self,
        block: SealedBlock<N::Block>,
        ctx: TreeCtx<'_, N>,
    ) -> ValidationOutcome<N> {
        let num_hash = block.num_hash();
        // SAFETY: Called from the engine tree's dedicated OS thread. See comment in
        // `validate_payload` above for details.
        let mut guard = self.inner.blocking_lock();

        if let Some(executed_block) = guard.try_flashblocks_cache_hit(&num_hash) {
            return Ok(executed_block);
        }

        // If flashblocks is enabled and engine validator is validating this new block,
        // it could mean that the default CL/EL sync received the block before the full
        // flashblocks sequence has received the last target flashblock.
        //
        // Note: the execution cache may be keyed to a flashblocks-built hash (not the
        // canonical hash), so the pre-warm from parent state may be a cache miss. This
        // is self-healing: `on_inserted_executed_block` below will re-key the cache
        // to the canonical hash for the next block's prewarming.
        debug!(
            target: "flashblocks::engine_validator",
            block_number = num_hash.number,
            block_hash = %num_hash.hash,
            "Flashblocks cache miss, engine validating block",
        );
        let executed_block = guard.engine_validator.validate_block(block, ctx)?;
        guard.try_advance_flashblocks_state(&executed_block);
        Ok(executed_block)
    }

    fn on_inserted_executed_block(&self, block: ExecutedBlock<N>) {
        // SAFETY: Called from the engine tree's dedicated OS thread. See comment in
        // `validate_payload` above for details.
        self.inner.blocking_lock().engine_validator.on_inserted_executed_block(block);
    }
}

impl<Provider, Evm, V, N, ChainSpec> XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>
where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718,
    N::Block: From<Block<N::SignedTx>>,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send>
        + Send
        + 'static,
    Provider: StateProviderFactory
        + HeaderProvider<Header = <N as NodePrimitives>::BlockHeader>
        + OverlayProviderFactory
        + BlockReader
        + StateReader
        + HashedPostStateProvider
        + Unpin
        + Clone
        + Send
        + 'static,
    ChainSpec: OpHardforks + Send + Sync + 'static,
    V: Send + 'static,
{
    /// Executes a flashblocks sequence through the inner
    /// [`FlashblockSequenceValidator`], holding the shared mutex for the full
    /// duration to prevent concurrent state-root computation with the engine.
    pub async fn execute_sequence<I: IntoIterator<Item = WithEncoded<Recovered<N::SignedTx>>>>(
        &self,
        args: BuildArgs<I>,
    ) -> eyre::Result<()> {
        let mut guard = self.inner.lock().await;
        let Some(fb_validator) = guard.fb_validator.as_mut() else {
            return Err(eyre::eyre!(
                "failed to execute flashblocks sequence, validator not initialized"
            ));
        };
        fb_validator.execute_sequence(args).await
    }
}

#[derive(Debug)]
struct XLayerEngineValidatorInner<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    // The underlying Reth's default engine validator.
    engine_validator: BasicEngineValidator<Provider, Evm, V>,
    /// Flashblocks validator, if flashblocks is enabled.
    fb_validator: Option<FlashblockSequenceValidator<N, Evm, Provider, ChainSpec>>,
    /// Flashblocks state cache, if flashblocks is enabled.
    fb_state: Option<FlashblockStateCache<N>>,
}

impl<Provider, Evm, V, N, ChainSpec> XLayerEngineValidatorInner<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    /// Checks the flashblocks confirm cache for a pre-validated block. Returns
    /// the cached `ExecutedBlock` on hit, skipping re-execution entirely.
    fn try_flashblocks_cache_hit(&self, num_hash: &BlockNumHash) -> Option<ExecutedBlock<N>> {
        let executed_block = self.fb_state.as_ref()?.get_executed_block_by_hash(&num_hash.hash)?;
        debug!(
            target: "flashblocks::engine_validator",
            block_number = num_hash.number,
            block_hash = %num_hash.hash,
            "Flashblocks cache hit, returning pre-validated block",
        );
        Some(executed_block)
    }

    /// Optimistically advances the flashblocks state cache after engine validation.
    /// Failures are logged but not propagated — the engine canonical stream is the
    /// source of truth.
    fn try_advance_flashblocks_state(&self, executed_block: &ExecutedBlock<N>) {
        if let Some(fb_state) = self.fb_state.as_ref()
            && let Err(e) = fb_state.try_handle_engine_block(executed_block.clone())
        {
            warn!(
                target: "flashblocks::engine_validator",
                %e,
                "Failed handle engine block on flashblocks state cache",
            );
        }
    }
}

/// Builder for [`XLayerEngineValidator`] that implements [`EngineValidatorBuilder`].
#[expect(clippy::type_complexity)]
#[derive(Clone, Debug)]
pub struct XLayerEngineValidatorBuilder<
    Provider,
    EV,
    Evm: ConfigureEvm,
    V,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
> {
    inner: BasicEngineValidatorBuilder<EV>,
    engine_validator: Arc<OnceLock<XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>>>,
    pvb: EV,
}

#[expect(clippy::type_complexity)]
impl<Provider, Evm: ConfigureEvm, V, N: NodePrimitives, ChainSpec: OpHardforks, EV: Default>
    XLayerEngineValidatorBuilder<Provider, EV, Evm, V, N, ChainSpec>
{
    pub fn new(
        inner: BasicEngineValidatorBuilder<EV>,
        engine_validator: Arc<OnceLock<XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>>>,
    ) -> Self {
        Self { inner, engine_validator, pvb: EV::default() }
    }
}

impl<Node, EV, ChainSpec> EngineValidatorBuilder<Node>
    for XLayerEngineValidatorBuilder<
        Node::Provider,
        EV,
        Node::Evm,
        EV::Validator,
        <Node::Types as NodeTypes>::Primitives,
        ChainSpec,
    >
where
    Node: FullNodeComponents<
        Evm: ConfigureEngineEvm<
            <<Node::Types as NodeTypes>::Payload as PayloadTypes>::ExecutionData,
        >,
    >,
    Node::Provider: CanonStateSubscriptions,
    EV: PayloadValidatorBuilder<Node>,
    EV::Validator:
        PayloadValidator<<Node::Types as NodeTypes>::Payload, Block = BlockTy<Node::Types>> + Clone,
    ChainSpec: OpHardforks + Send + Sync + Clone + 'static,
    ReceiptTy<Node::Types>: FlashblockReceipt,
    BlockTy<Node::Types>: From<Block<TxTy<Node::Types>>>,
    <Node::Evm as ConfigureEvm>::NextBlockEnvCtx: From<OpFlashblockPayloadBase> + Unpin + Send,
{
    type EngineValidator = XLayerEngineValidator<
        Node::Provider,
        Node::Evm,
        EV::Validator,
        <Node::Types as NodeTypes>::Primitives,
        ChainSpec,
    >;

    async fn build_tree_validator(
        self,
        ctx: &AddOnsContext<'_, Node>,
        tree_config: reth_engine_primitives::TreeConfig,
        changeset_cache: ChangesetCache,
    ) -> eyre::Result<Self::EngineValidator> {
        let validator = self.pvb.build(ctx).await?;
        let engine_validator = XLayerEngineValidator::new(
            self.inner.build_tree_validator(ctx, tree_config, changeset_cache).await?,
            validator,
        );
        self.engine_validator
            .set(engine_validator.clone())
            .map_err(|_| eyre::eyre!("failed to set engine validator"))?;
        Ok(engine_validator)
    }
}
