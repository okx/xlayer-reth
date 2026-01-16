//! XLayer EVM configuration with Poseidon precompile support.

use core::marker::PhantomData;
use std::sync::Arc;

use alloy_consensus::Header;
use alloy_eips::Decodable2718;
use alloy_primitives::U256;
use op_alloy_consensus::EIP1559ParamError;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth_chainspec::EthChainSpec;
use reth_evm::{
    ConfigureEngineEvm, ConfigureEvm, EvmEnv, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor,
};
use reth_primitives_traits::node::{BlockTy, HeaderTy};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{
    revm_spec_by_timestamp_after_bedrock, OpBlockAssembler, OpBlockExecutionCtx,
    OpBlockExecutorFactory, OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder,
};
use reth_optimism_primitives::OpPrimitives;
use reth_primitives_traits::{SealedBlock, SealedHeader, SignedTransaction, TxTy, WithEncoded};
use reth_storage_errors::any::AnyError;
use revm::{
    context::{BlockEnv, CfgEnv},
    context_interface::block::BlobExcessGasAndPrice,
    primitives::hardfork::SpecId,
};

use crate::evm_factory::XLayerEvmFactory;

/// XLayer EVM configuration wrapper.
#[derive(Debug, Clone)]
pub struct XLayerEvmConfig {
    inner: OpEvmConfig<OpChainSpec, OpPrimitives, OpRethReceiptBuilder, XLayerEvmFactory>,
}

impl XLayerEvmConfig {
    /// Create XLayer EVM config.
    pub fn new(chain_spec: Arc<OpChainSpec>) -> Self {
        tracing::info!(
            target: "xlayer::evm",
            "📦 Creating XLayer EVM config with custom precompiles"
        );

        let receipt_builder = OpRethReceiptBuilder::default();
        let executor_factory =
            OpBlockExecutorFactory::new(receipt_builder, chain_spec.clone(), XLayerEvmFactory);
        let block_assembler = OpBlockAssembler::new(chain_spec);

        let inner = OpEvmConfig {
            executor_factory,
            block_assembler,
            _pd: PhantomData,
        };

        Self { inner }
    }

}

/// Create XLayer EVM config.
pub fn xlayer_evm_config(chain_spec: Arc<OpChainSpec>) -> XLayerEvmConfig {
    XLayerEvmConfig::new(chain_spec)
}

impl ConfigureEvm for XLayerEvmConfig {
    type Primitives = OpPrimitives;
    type Error = EIP1559ParamError;
    type NextBlockEnvCtx = OpNextBlockEnvAttributes;
    type BlockExecutorFactory =
        OpBlockExecutorFactory<OpRethReceiptBuilder, Arc<OpChainSpec>, XLayerEvmFactory>;
    type BlockAssembler = OpBlockAssembler<OpChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.inner.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.inner.block_assembler
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<BlockTy<Self::Primitives>>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<HeaderTy<Self::Primitives>>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

impl ConfigureEngineEvm<OpExecutionData> for XLayerEvmConfig {
    fn evm_env_for_payload(
        &self,
        payload: &OpExecutionData,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        let timestamp = payload.payload.timestamp();
        let block_number = payload.payload.block_number();

        let spec = revm_spec_by_timestamp_after_bedrock(self.inner.chain_spec(), timestamp);

        let cfg_env = CfgEnv::new().with_chain_id(self.inner.chain_spec().chain().id()).with_spec(spec);

        let blob_excess_gas_and_price = spec
            .into_eth_spec()
            .is_enabled_in(SpecId::CANCUN)
            .then_some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 1 });

        let block_env = BlockEnv {
            number: U256::from(block_number),
            beneficiary: payload.payload.as_v1().fee_recipient,
            timestamp: U256::from(timestamp),
            difficulty: if spec.into_eth_spec() >= SpecId::MERGE {
                U256::ZERO
            } else {
                payload.payload.as_v1().prev_randao.into()
            },
            prevrandao: (spec.into_eth_spec() >= SpecId::MERGE)
                .then(|| payload.payload.as_v1().prev_randao),
            gas_limit: payload.payload.as_v1().gas_limit,
            basefee: payload.payload.as_v1().base_fee_per_gas.to(),
            // EIP-4844 excess blob gas of this block, introduced in Cancun
            blob_excess_gas_and_price,
        };

        Ok(EvmEnv { cfg_env, block_env })
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a OpExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        Ok(OpBlockExecutionCtx {
            parent_hash: payload.parent_hash(),
            parent_beacon_block_root: payload.sidecar.parent_beacon_block_root(),
            extra_data: payload.payload.as_v1().extra_data.clone(),
        })
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &OpExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        Ok(payload.payload.transactions().clone().into_iter().map(|encoded| {
            let tx = TxTy::<Self::Primitives>::decode_2718_exact(encoded.as_ref())
                .map_err(AnyError::new)?;
            let signer = tx.try_recover().map_err(AnyError::new)?;
            Ok::<_, AnyError>(WithEncoded::new(encoded, tx.with_signer(signer)))
        }))
    }
}

