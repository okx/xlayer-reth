use crate::helper::{
    to_block_receipts, to_rpc_block, to_rpc_transaction, to_rpc_transaction_from_bar_and_index,
};
use futures::StreamExt;
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};
use tokio_stream::wrappers::WatchStream;
use tracing::*;

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, Bytes, TxHash, B256, U256};
use alloy_rpc_types_eth::{
    state::{EvmOverrides, StateOverride},
    BlockOverrides, Filter, Index, Log, TransactionInfo,
};
use op_alloy_network::Optimism;
use op_alloy_rpc_types::OpTransactionRequest;

use reth_chain_state::CanonStateSubscriptions;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::eth::OpEthApi;
use reth_primitives_traits::{BlockBody, NodePrimitives, SealedHeaderFor, SignerRecoverable};
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_rpc::eth::EthFilter;
use reth_rpc_convert::RpcTransaction;
use reth_rpc_eth_api::{
    helpers::{estimate::EstimateCall, EthBlocks, EthCall, EthState, EthTransactions, FullEthApi},
    EthApiServer, EthApiTypes, RpcBlock, RpcNodeCore, RpcReceipt,
};
use reth_rpc_eth_types::{block::convert_transaction_receipt, error::FromEvmError, EthApiError};
use reth_rpc_server_types::result::ToRpcResult;
use reth_storage_api::{StateProvider, StateProviderBox, StateProviderFactory};

use xlayer_flashblocks::FlashblockStateCache;

/// Eth API override for flashblocks RPC integration.
#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
pub trait FlashblocksEthApiOverride {
    // ----------------- Block apis -----------------
    /// Returns the current block number, with the flashblocks state cache overlay.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<U256>;

    /// Returns block by number, with the flashblock state cache overlay support for pending and
    /// confirmed blocks.
    #[method(name = "getBlockByNumber")]
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>>;

    /// Returns block by block hash, with the flashblock state cache overlay support for pending
    /// and confirmed blocks.
    #[method(name = "getBlockByHash")]
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<RpcBlock<Optimism>>>;

    /// Returns all the receipts in a block by block number, with the flashblock state cache
    /// overlay support for pending and confirmed blocks.
    #[method(name = "getBlockReceipts")]
    async fn block_receipts(
        &self,
        block_id: BlockNumberOrTag,
    ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>>;

    /// Returns the number of transactions in a block by block number, with the flashblock state
    /// cache overlay support for pending and confirmed blocks.
    #[method(name = "getBlockTransactionCountByNumber")]
    async fn block_transaction_count_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> RpcResult<Option<U256>>;

    /// Returns the number of transactions in a block by block hash, with the flashblock state
    /// cache overlay support for pending and confirmed blocks.
    #[method(name = "getBlockTransactionCountByHash")]
    async fn block_transaction_count_by_hash(&self, hash: B256) -> RpcResult<Option<U256>>;

    // ----------------- Transaction apis -----------------
    /// Returns the information about a transaction requested by transaction hash, with the
    /// flashblock state cache overlay support for pending and confirmed blocks.
    #[method(name = "getTransactionByHash")]
    async fn transaction_by_hash(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns the EIP-2718 encoded transaction if it exists, with the flashblock state cache
    /// overlay support for pending and confirmed blocks.
    #[method(name = "getRawTransactionByHash")]
    async fn raw_transaction_by_hash(&self, hash: B256) -> RpcResult<Option<Bytes>>;

    /// Returns the receipt of a transaction by transaction hash, with the flashblock state cache
    /// overlay support for pending and confirmed blocks.
    #[method(name = "getTransactionReceipt")]
    async fn transaction_receipt(&self, hash: TxHash) -> RpcResult<Option<RpcReceipt<Optimism>>>;

    /// Returns information about a raw transaction by block hash and transaction index position,
    /// with the flashblock state cache overlay support for pending and confirmed blocks.
    #[method(name = "getTransactionByBlockHashAndIndex")]
    async fn transaction_by_block_hash_and_index(
        &self,
        block_hash: B256,
        index: Index,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns information about a transaction by block number and transaction index position,
    /// with the flashblock state cache overlay support for pending and confirmed blocks.
    #[method(name = "getTransactionByBlockNumberAndIndex")]
    async fn transaction_by_block_number_and_index(
        &self,
        number: BlockNumberOrTag,
        index: Index,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>>;

    /// Returns information about a raw transaction by block hash and transaction index position,
    /// with the flashblock state cache overlay support for pending and confirmed blocks.
    #[method(name = "getRawTransactionByBlockHashAndIndex")]
    async fn raw_transaction_by_block_hash_and_index(
        &self,
        hash: B256,
        index: Index,
    ) -> RpcResult<Option<Bytes>>;

    /// Returns information about a raw transaction by block number and transaction index position,
    /// with the flashblock state cache overlay support for pending and confirmed blocks.
    #[method(name = "getRawTransactionByBlockNumberAndIndex")]
    async fn raw_transaction_by_block_number_and_index(
        &self,
        number: BlockNumberOrTag,
        index: Index,
    ) -> RpcResult<Option<Bytes>>;

    /// Sends a signed transaction and awaits the transaction receipt, with the flashblock state cache
    /// overlay support for pending and confirmed blocks.
    ///
    /// This will return a timeout error if the transaction isn't included within some time period.
    #[method(name = "sendRawTransactionSync")]
    async fn send_raw_transaction_sync(&self, bytes: Bytes) -> RpcResult<RpcReceipt<Optimism>>;

    // ----------------- State apis -----------------
    /// Executes a new message call immediately without creating a transaction on the block chain,
    /// with the flashblock state cache overlay support for pending and confirmed block states.
    #[method(name = "call")]
    async fn call(
        &self,
        transaction: OpTransactionRequest,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<Bytes>;

    /// Generates and returns an estimate of how much gas is necessary to allow the transaction to
    /// complete, with the flashblock state cache overlay support for pending and confirmed block
    /// states.
    #[method(name = "estimateGas")]
    async fn estimate_gas(
        &self,
        transaction: OpTransactionRequest,
        block_number: Option<BlockId>,
        overrides: Option<StateOverride>,
    ) -> RpcResult<U256>;
    /// Returns the balance of the account of given address, with the flashblock state cache
    /// overlay support for pending and confirmed block states.

    #[method(name = "getBalance")]
    async fn balance(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<U256>;

    /// Returns the number of transactions sent from an address at given block number, with the
    /// flashblock state cache overlay support for pending and confirmed block states.
    #[method(name = "getTransactionCount")]
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256>;

    /// Returns code at a given address at given block number, with the flashblock state cache
    /// overlay support for pending and confirmed block states.
    #[method(name = "getCode")]
    async fn get_code(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<Bytes>;

    /// Returns the value from a storage position at a given address, with the flashblock state
    /// cache overlay support for pending and confirmed block states.
    #[method(name = "getStorageAt")]
    async fn storage_at(
        &self,
        address: Address,
        slot: U256,
        block_number: Option<BlockId>,
    ) -> RpcResult<B256>;
}

/// Extended Eth API with flashblocks cache overlay.
#[derive(Debug)]
pub struct XLayerEthApiExt<Eth: EthApiTypes> {
    eth_api: OpEthApi<Eth>,
    flashblocks_state: FlashblockStateCache<OpPrimitives>,
}

impl<Eth: EthApiTypes> XLayerEthApiExt<Eth> {
    /// Creates a new [`XLayerEthApiExt`].
    pub fn new(
        eth_api: OpEthApi<Eth>,
        flashblocks_state: FlashblockStateCache<OpPrimitives>,
    ) -> Self {
        Self { eth_api, flashblocks_state }
    }
}

#[async_trait]
impl<Eth> FlashblocksEthApiOverrideServer for XLayerEthApiExt<Eth>
where
    Eth: FullEthApi<NetworkTypes = Optimism> + Send + Sync + 'static,
    jsonrpsee_types::error::ErrorObject<'static>: From<Eth::Error>,
{
    // ----------------- Block apis -----------------
    /// Handler for: `eth_blockNumber`
    async fn block_number(&self) -> RpcResult<U256> {
        trace!(target: "rpc::eth", "Serving eth_blockNumber");
        let fb_height = self.flashblocks_state.get_confirm_height();
        let canon_height = self.eth_api.block_number().await?;
        Ok(U256::from(std::cmp::max(fb_height, canon_height)))
    }

    /// Handler for: `eth_getBlockByNumber`
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<RpcBlock<Optimism>>> {
        trace!(target: "rpc::eth", ?number, ?full, "Serving eth_getBlockByNumber");
        if let Some(bar) = self.flashblocks_state.get_rpc_block(number) {
            return to_rpc_block::<Eth>(&bar, full, self.eth_api.converter())
                .map(Some)
                .map_err(Into::into);
        }
        self.eth_api.block_by_number(number, full).await
    }

    /// Handler for: `eth_getBlockByHash`
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<RpcBlock<Optimism>>> {
        trace!(target: "rpc::eth", ?hash, ?full, "Serving eth_getBlockByHash");
        if let Some(bar) = self.flashblocks_state.get_block_by_hash(&hash) {
            return to_rpc_block::<Eth>(&bar, full, self.eth_api.converter())
                .map(Some)
                .map_err(Into::into);
        }
        self.eth_api.block_by_hash(hash, full).await
    }

    /// Handler for: `eth_getBlockReceipts`
    async fn block_receipts(
        &self,
        block_id: BlockNumberOrTag,
    ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>> {
        trace!(target: "rpc::eth", ?block_id, "Serving eth_getBlockReceipts");
        if let Some(bar) = self.flashblocks_state.get_rpc_block(block_id) {
            return to_block_receipts::<Eth>(&bar, self.eth_api.converter())
                .map(Some)
                .map_err(Into::into);
        }
        self.eth_api.block_receipts(block_id).await
    }

    /// Handler for: `eth_getBlockTransactionCountByNumber`
    async fn block_transaction_count_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> RpcResult<Option<U256>> {
        trace!(target: "rpc::eth", ?number, "Serving eth_getBlockTransactionCountByNumber");
        if let Some(bar) = self.flashblocks_state.get_rpc_block(number) {
            let count = bar.block.body().transaction_count();
            return Ok(Some(U256::from(count)));
        }
        self.eth_api.block_transaction_count_by_number(number).await
    }

    /// Handler for: `eth_getUncleCountByBlockHash`
    async fn block_transaction_count_by_hash(&self, hash: B256) -> RpcResult<Option<U256>> {
        trace!(target: "rpc::eth", ?hash, "Serving eth_getBlockTransactionCountByHash");
        if let Some(bar) = self.flashblocks_state.get_block_by_hash(&hash) {
            let count = bar.block.body().transaction_count();
            return Ok(Some(U256::from(count)));
        }
        self.eth_api.block_transaction_count_by_hash(hash).await
    }

    // ----------------- Transaction apis -----------------
    /// Handler for: `eth_getTransactionByHash`
    async fn transaction_by_hash(
        &self,
        hash: TxHash,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        trace!(target: "rpc::eth", ?hash, "Serving eth_getTransactionByHash");
        if let Some((info, bar)) = self.flashblocks_state.get_tx_info(&hash) {
            return Ok(Some(to_rpc_transaction::<Eth>(&info, &bar, self.eth_api.converter())?));
        }
        self.eth_api.transaction_by_hash(hash).await
    }

    /// Handler for: `eth_getRawTransactionByHash`
    async fn raw_transaction_by_hash(&self, hash: B256) -> RpcResult<Option<Bytes>> {
        trace!(target: "rpc::eth", ?hash, "Serving eth_getRawTransactionByHash");
        if let Some((info, _)) = self.flashblocks_state.get_tx_info(&hash) {
            return Ok(Some(info.tx.encoded_2718().into()));
        }
        self.eth_api.raw_transaction_by_hash(hash).await
    }

    /// Handler for: `eth_getTransactionReceipt`
    async fn transaction_receipt(&self, hash: TxHash) -> RpcResult<Option<RpcReceipt<Optimism>>> {
        trace!(target: "rpc::eth", ?hash, "Serving eth_getTransactionReceipt");
        if let Some((_, bar)) = self.flashblocks_state.get_tx_info(&hash)
            && let Some(Ok(receipt)) =
                bar.find_and_convert_transaction_receipt(hash, self.eth_api.converter())
        {
            return Ok(Some(receipt));
        }
        self.eth_api.transaction_receipt(hash).await
    }

    /// Handler for: `eth_getTransactionByBlockHashAndIndex`
    async fn transaction_by_block_hash_and_index(
        &self,
        block_hash: B256,
        index: Index,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        trace!(target: "rpc::eth", ?block_hash, ?index, "Serving eth_getTransactionByBlockHashAndIndex");
        if let Some(bar) = self.flashblocks_state.get_block_by_hash(&block_hash) {
            return to_rpc_transaction_from_bar_and_index::<Eth>(
                &bar,
                index.into(),
                self.eth_api.converter(),
            )
            .map_err(Into::into);
        }
        self.eth_api.transaction_by_block_hash_and_index(block_hash, index).await
    }

    /// Handler for: `eth_getTransactionByBlockNumberAndIndex`
    async fn transaction_by_block_number_and_index(
        &self,
        number: BlockNumberOrTag,
        index: Index,
    ) -> RpcResult<Option<RpcTransaction<Optimism>>> {
        trace!(target: "rpc::eth", ?number, ?index, "Serving eth_getTransactionByBlockNumberAndIndex");
        if let Some(bar) = self.flashblocks_state.get_rpc_block(number) {
            return to_rpc_transaction_from_bar_and_index::<Eth>(
                &bar,
                index.into(),
                self.eth_api.converter(),
            )
            .map_err(Into::into);
        }
        self.eth_api.transaction_by_block_number_and_index(number, index).await
    }

    /// Handler for: `eth_getRawTransactionByBlockHashAndIndex`
    async fn raw_transaction_by_block_hash_and_index(
        &self,
        hash: B256,
        index: Index,
    ) -> RpcResult<Option<Bytes>> {
        trace!(target: "rpc::eth", ?hash, ?index, "Serving eth_getRawTransactionByBlockHashAndIndex");
        if let Some(bar) = self.flashblocks_state.get_block_by_hash(&hash)
            && let Some(tx) = bar.block.body().transactions().nth(index.into())
        {
            return Ok(Some(tx.encoded_2718().into()));
        }
        self.eth_api.raw_transaction_by_block_hash_and_index(hash, index).await
    }

    /// Handler for: `eth_getRawTransactionByBlockNumberAndIndex`
    async fn raw_transaction_by_block_number_and_index(
        &self,
        number: BlockNumberOrTag,
        index: Index,
    ) -> RpcResult<Option<Bytes>> {
        trace!(target: "rpc::eth", ?number, ?index, "Serving eth_getRawTransactionByBlockNumberAndIndex");
        if let Some(bar) = self.flashblocks_state.get_rpc_block(number)
            && let Some(tx) = bar.block.body().transactions().nth(index.into())
        {
            return Ok(Some(tx.encoded_2718().into()));
        }
        self.eth_api.raw_transaction_by_block_number_and_index(number, index).await
    }

    /// Handler for: `eth_sendRawTransactionSync`
    async fn send_raw_transaction_sync(&self, tx: Bytes) -> RpcResult<RpcReceipt<Optimism>> {
        trace!(target: "rpc::eth", ?tx, "Serving eth_sendRawTransactionSync");
        let timeout_duration = EthTransactions::send_raw_transaction_sync_timeout(&self.eth_api);
        let hash =
            EthTransactions::send_raw_transaction(&self.eth_api, tx).await.map_err(Into::into)?;
        let converter = self.eth_api.converter();

        let mut canonical_stream = self.eth_api.provider().canonical_state_stream();
        let mut flashblock_stream =
            WatchStream::new(self.flashblocks_state.subscribe_pending_sequence());

        tokio::time::timeout(timeout_duration, async {
            loop {
                tokio::select! {
                    biased;
                    // check if the tx was preconfirmed in the latest flashblocks pending sequence
                    pending = flashblock_stream.next() => {
                        if let Some(pending_sequence) = pending.flatten() {
                            let bar = pending_sequence.get_block_and_receipts();
                            if let Some(receipt) =
                                bar.find_and_convert_transaction_receipt(hash, converter)
                            {
                                return receipt;
                            }
                        }
                    }
                    // Listen for regular canonical block updates for inclusion
                    canonical_notification = canonical_stream.next() => {
                        if let Some(notification) = canonical_notification {
                            let chain = notification.committed();
                            if let Some((block, tx, receipt, all_receipts)) =
                                chain.find_transaction_and_receipt_by_hash(hash)
                            {
                                if let Some(receipt) = convert_transaction_receipt(
                                    block,
                                    all_receipts,
                                    tx,
                                    receipt,
                                    converter,
                                )
                                .transpose()
                                .map_err(Into::into)?
                                {
                                    return Ok(receipt);
                                }
                            }
                        } else {
                            // Canonical stream ended
                            break;
                        }
                    }
                }
            }
            Err(EthApiError::TransactionConfirmationTimeout { hash, duration: timeout_duration }
                .into())
        })
        .await
        .unwrap_or_else(|_elapsed| {
            Err(EthApiError::TransactionConfirmationTimeout { hash, duration: timeout_duration }
                .into())
        })
    }

    // ----------------- State apis -----------------
    /// Handler for: `eth_call`
    async fn call(
        &self,
        transaction: OpTransactionRequest,
        block_number: Option<BlockId>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<Bytes> {
        trace!(target: "rpc::eth", ?transaction, ?block_number, ?state_overrides, ?block_overrides, "Serving eth_call");
        if let Some((state, header)) = self.get_flashblock_state_provider_by_id(block_number)? {
            let evm_env = self.eth_api.evm_env_for_header(&header).map_err(Into::into)?;
            let mut db = State::builder().with_database(StateProviderDatabase::new(state)).build();
            let (evm_env, tx_env) = self
                .eth_api
                .prepare_call_env(
                    evm_env,
                    transaction,
                    &mut db,
                    EvmOverrides::new(state_overrides, block_overrides),
                )
                .map_err(Into::into)?;
            let res = EthCall::transact(&self.eth_api, db, evm_env, tx_env).map_err(Into::into)?;
            return <Eth as EthApiTypes>::Error::ensure_success(res.result).map_err(Into::into);
        }
        self.eth_api.call(transaction, block_number, state_overrides, block_overrides).await
    }

    /// Handler for: `eth_estimateGas`
    async fn estimate_gas(
        &self,
        transaction: OpTransactionRequest,
        block_number: Option<BlockId>,
        overrides: Option<StateOverride>,
    ) -> RpcResult<U256> {
        trace!(target: "rpc::eth", ?transaction, ?block_number, "Serving eth_estimateGas");
        if let Some((state, header)) = self.get_flashblock_state_provider_by_id(block_number)? {
            let evm_env = self.eth_api.evm_env_for_header(&header).map_err(Into::into)?;
            return self
                .eth_api
                .estimate_gas_with(evm_env, transaction, state, overrides)
                .map_err(Into::into);
        }
        self.eth_api.estimate_gas(transaction, block_number, overrides).await
    }

    /// Handler for: `eth_getBalance`
    async fn balance(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<U256> {
        trace!(target: "rpc::eth", ?address, ?block_number, "Serving eth_getBalance");
        if let Some((state, _)) = self.get_flashblock_state_provider_by_id(block_number)? {
            return Ok(state.account_balance(&address).to_rpc_result()?.unwrap_or_default());
        }
        self.eth_api.balance(address, block_number).await
    }

    /// Handler for: `eth_getTransactionCount`
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256> {
        trace!(target: "rpc::eth", ?address, ?block_number, "Serving eth_getTransactionCount");
        if let Some((state, _)) = self.get_flashblock_state_provider_by_id(block_number)? {
            return Ok(U256::from(
                state.account_nonce(&address).to_rpc_result()?.unwrap_or_default(),
            ));
        }
        self.eth_api.transaction_count(address, block_number).await
    }

    /// Handler for: `eth_getCode`
    async fn get_code(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<Bytes> {
        trace!(target: "rpc::eth", ?address, ?block_number, "Serving eth_getCode");
        if let Some((state, _)) = self.get_flashblock_state_provider_by_id(block_number)? {
            return Ok(state
                .account_code(&address)
                .to_rpc_result()?
                .map(|code| code.original_bytes())
                .unwrap_or_default());
        }
        self.eth_api.get_code(address, block_number).await
    }

    /// Handler for: `eth_getStorageAt`
    async fn storage_at(
        &self,
        address: Address,
        slot: U256,
        block_number: Option<BlockId>,
    ) -> RpcResult<B256> {
        trace!(target: "rpc::eth", ?address, ?slot, ?block_number, "Serving eth_getStorageAt");
        if let Some((state, _)) = self.get_flashblock_state_provider_by_id(block_number)? {
            let storage_key = B256::new(slot.to_be_bytes());
            return Ok(B256::new(
                state
                    .storage(address, storage_key)
                    .to_rpc_result()?
                    .unwrap_or_default()
                    .to_be_bytes(),
            ));
        }
        self.eth_api.storage_at(address, slot, block_number).await
    }
}

impl<Eth> XLayerEthApiExt<Eth>
where
    Eth: FullEthApi<NetworkTypes = Optimism> + Send + Sync + 'static,
{
    /// Returns a `StateProvider` overlaying flashblock execution state on top of canonical state
    /// for the given block ID. Returns `None` if the block is not in the flashblocks cache.
    fn get_flashblock_state_provider_by_id(
        &self,
        block_id: Option<BlockId>,
    ) -> RpcResult<Option<(StateProviderBox, SealedHeaderFor<OpPrimitives>)>> {
        let canon_state = self.eth_api.provider().latest().to_rpc_result()?;
        Ok(self.flashblocks_state.get_state_provider_by_id(block_id, canon_state))
    }
}
