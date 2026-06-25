use crate::{
    execution::{FlashblockSequenceValidator, OverlayProviderFactory},
    BuildArgs, FlashblockReceipt, FlashblockStateCache,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use alloy_consensus::Block;
use alloy_eips::{eip2718::Encodable2718, BlockNumHash};
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;

use reth_chain_state::ExecutedBlock;
use reth_engine_primitives::{ExecutionPayload, PayloadValidator};
use reth_engine_tree::tree::{
    payload_processor::multiproof::StateRootHandle,
    payload_validator::{TreeCtx, ValidationOutcome},
    BasicEngineValidator, CacheWaitDurations, EngineApiTreeState, EngineValidator,
    PayloadProcessor, SavedCache, ValidationOutput, WaitForCaches,
};
use reth_evm::{ConfigureEngineEvm, ConfigureEvm};
use reth_node_api::{
    AddOnsContext, BlockTy, FullNodeComponents, NodeTypes, PayloadTypes, ReceiptTy, TxTy,
};
use reth_node_builder::rpc::{
    BasicEngineValidatorBuilder, EngineValidatorBuilder, PayloadValidatorBuilder,
};
use reth_optimism_forks::OpHardforks;
use reth_payload_primitives::{
    BuiltPayload, BuiltPayloadExecutedBlock, InvalidPayloadAttributesError, NewPayloadError,
};
use reth_primitives_traits::{
    transaction::{signed::SignedTransaction, TxHashRef},
    Block as BlockTrait, BlockBody, NodePrimitives, Recovered, SealedBlock, WithEncoded,
};
use reth_provider::{
    BlockReader, CanonStateSubscriptions, HashedPostStateProvider, HeaderProvider, ProviderResult,
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
        let sealed_block = self.validator.convert_payload_to_block(payload.clone()).ok();

        // SAFETY: `blocking_lock()` is safe here because the engine tree runs on a dedicated
        // OS thread (`spawn_os_thread("engine", ...)` in `reth-engine-tree`), not a tokio
        // worker. These `EngineValidator` trait methods are only called from
        // `insert_block_or_payload` within that thread's synchronous `run()` loop.
        // The init helpers (`set_flashblocks`, `get_changeset_cache`, `get_payload_processor`)
        // use `block_in_place` instead because they are called from async context during
        // node startup.
        let mut guard = self.inner.blocking_lock();

        if let Some(executed_block) = guard.try_flashblocks_cache_hit(&num_hash) {
            return Ok(ValidationOutput::new(executed_block, None));
        }

        // Hot path: use pending FB sequence state and the warm FB sequence validator
        if let Some(ref block) = sealed_block
            && let Some(args) = guard.try_get_flashblocks_buildable_args(block)
            && let Some(executed_block) = guard.engine_execute_sequence(args, num_hash.hash)
        {
            return Ok(ValidationOutput::new(executed_block, None));
        }

        info!(
            target: "flashblocks::engine_validator",
            block_number = num_hash.number,
            block_hash = %num_hash.hash,
            "Failed to build with flashblocks pending sequence, defaulting to engine validation",
        );

        // Drop any in-flight flashblocks sparse-trie task so the `PayloadProcessor`'s
        // preserved sparse trie is released
        guard.drop_flashblocks_state_root_handle();

        let output = guard.engine_validator.validate_payload(payload, ctx)?;
        guard.try_advance_flashblocks_state(&output.executed_block);
        Ok(output)
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
            return Ok(ValidationOutput::new(executed_block, None));
        }

        // Hot path: use pending FB sequence state and the warm FB sequence validator
        if let Some(args) = guard.try_get_flashblocks_buildable_args(&block)
            && let Some(executed_block) = guard.engine_execute_sequence(args, num_hash.hash)
        {
            return Ok(ValidationOutput::new(executed_block, None));
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
            "Failed to build with flashblocks pending sequence, defaulting to engine validation",
        );

        // Drop any in-flight flashblocks sparse-trie task so the `PayloadProcessor`'s
        // preserved sparse trie is released
        guard.drop_flashblocks_state_root_handle();

        let output = guard.engine_validator.validate_block(block, ctx)?;
        guard.try_advance_flashblocks_state(&output.executed_block);
        Ok(output)
    }

    fn on_inserted_executed_block(
        &self,
        block: BuiltPayloadExecutedBlock<N>,
        state: &EngineApiTreeState<N>,
    ) -> ProviderResult<ExecutedBlock<N>> {
        // SAFETY: Called from the engine tree's dedicated OS thread. See comment in
        // `validate_payload` above for details.
        self.inner.blocking_lock().engine_validator.on_inserted_executed_block(block, state)
    }

    fn cache_for(&self, block_hash: alloy_primitives::B256) -> Option<SavedCache> {
        // SAFETY: Called from the engine tree's dedicated OS thread.
        self.inner.blocking_lock().engine_validator.cache_for(block_hash)
    }

    fn sparse_trie_handle_for(
        &self,
        parent_hash: alloy_primitives::B256,
        parent_state_root: alloy_primitives::B256,
        state: &EngineApiTreeState<N>,
    ) -> Option<StateRootHandle> {
        // SAFETY: Called from the engine tree's dedicated OS thread.
        self.inner.blocking_lock().engine_validator.sparse_trie_handle_for(
            parent_hash,
            parent_state_root,
            state,
        )
    }
}

impl<Provider, Evm, V, N, ChainSpec> WaitForCaches
    for XLayerEngineValidator<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    fn wait_for_caches(&self) -> CacheWaitDurations {
        // SAFETY: Called from the engine tree's dedicated OS thread.
        self.inner.blocking_lock().engine_validator.wait_for_caches()
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
        // On failure, drop the state root task handle so the `PayloadProcessor`'s
        // polluted sparse trie cache is released, then propagate the error.
        fb_validator.execute_sequence(args).await.inspect_err(|_| {
            guard.drop_flashblocks_state_root_handle();
        })
    }
}

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

// Manual `Debug` impl: `BasicEngineValidator` no longer implements `Debug` (it now holds a
// boxed state-root-input closure), so we can't `#[derive(Debug)]` here.
impl<Provider, Evm, V, N, ChainSpec> std::fmt::Debug
    for XLayerEngineValidatorInner<Provider, Evm, V, N, ChainSpec>
where
    Evm: ConfigureEvm,
    N: NodePrimitives,
    ChainSpec: OpHardforks,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XLayerEngineValidatorInner").finish_non_exhaustive()
    }
}

impl<Provider, Evm, V, N, ChainSpec> XLayerEngineValidatorInner<Provider, Evm, V, N, ChainSpec>
where
    N: NodePrimitives,
    N::Receipt: FlashblockReceipt,
    N::SignedTx: Encodable2718 + SignedTransaction,
    N::Block: From<Block<N::SignedTx>>,
    <<N::Block as BlockTrait>::Body as BlockBody>::Transaction: TxHashRef,
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

    /// Drops any in-flight flashblocks state root task handle so the shared
    /// `PayloadProcessor`'s preserved sparse trie is released.
    fn drop_flashblocks_state_root_handle(&mut self) {
        if let Some(fb) = self.fb_validator.as_mut() {
            fb.drop_state_root_handle();
        }
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

    /// Try using the current flashblocks sequence to generate buildable args to be
    /// validated by the flashblocks sequence validator's warm caches, using the
    /// default payload as the synthesized end.
    #[expect(clippy::type_complexity)]
    fn try_get_flashblocks_buildable_args(
        &mut self,
        block: &SealedBlock<N::Block>,
    ) -> Option<BuildArgs<Vec<WithEncoded<Recovered<N::SignedTx>>>>> {
        let pending = self.fb_state.as_ref()?.get_pending_sequence()?;
        pending
            .try_insert_default_payload(block.clone())
            .inspect_err(|error| {
                info!(
                    target: "flashblocks::engine_validator",
                    %error,
                    "incoming default payload does not extend current pending sequence",
                );
            })
            .ok()
    }

    /// Drives the flashblocks sequence validator's warm pipeline to a sequence-end
    /// commit using the synthesized [`BuildArgs`], then re-checks the executed block
    /// with the canonical hash.
    fn engine_execute_sequence(
        &mut self,
        args: BuildArgs<Vec<WithEncoded<Recovered<N::SignedTx>>>>,
        canon_hash: alloy_primitives::B256,
    ) -> Option<ExecutedBlock<N>> {
        let fb_validator = self.fb_validator.as_mut()?;
        let runtime = fb_validator.runtime().handle().clone();
        if let Err(e) = runtime.block_on(fb_validator.execute_sequence(args)) {
            info!(
                target: "flashblocks::engine_validator",
                %e,
                "execute_sequence failed on incremental building from engine default payload",
            );
            return None;
        }
        self.fb_state.as_ref()?.get_executed_block_by_hash(&canon_hash)
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
