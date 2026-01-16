//! XLayer EVM factory that injects custom precompiles (Poseidon).

use op_revm::{OpContext, OpHaltReason, OpSpecId, OpTransaction, OpTransactionError};
use reth_evm::{precompiles::PrecompilesMap, Database, Evm, EvmEnv, EvmFactory};
use reth_optimism_evm::{OpEvm, OpEvmFactory};
use reth_optimism_forks::OpHardfork;
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::EVMError,
    inspector::NoOpInspector,
    interpreter::interpreter::EthInterpreter,
    Inspector,
};

use crate::factory::xlayer_precompiles;

/// Convert op-revm spec id to OP hardfork.
fn hardfork_from_spec_id(spec_id: OpSpecId) -> OpHardfork {
    match spec_id {
        OpSpecId::BEDROCK => OpHardfork::Bedrock,
        OpSpecId::REGOLITH => OpHardfork::Regolith,
        OpSpecId::CANYON => OpHardfork::Canyon,
        OpSpecId::ECOTONE => OpHardfork::Ecotone,
        OpSpecId::FJORD => OpHardfork::Fjord,
        OpSpecId::GRANITE => OpHardfork::Granite,
        OpSpecId::HOLOCENE => OpHardfork::Holocene,
        OpSpecId::ISTHMUS => OpHardfork::Isthmus,
        OpSpecId::JOVIAN => OpHardfork::Jovian,
        OpSpecId::INTEROP => OpHardfork::Interop,
        // Unknown OP hardforks (e.g. future) default to Jovian to keep Poseidon enabled.
        OpSpecId::OSAKA => OpHardfork::Jovian,
    }
}

fn xlayer_precompiles_map(spec_id: OpSpecId) -> PrecompilesMap {
    let hardfork = hardfork_from_spec_id(spec_id);
    PrecompilesMap::from_static(xlayer_precompiles(hardfork))
}

/// Custom EVM factory that injects XLayer precompiles.
#[derive(Clone, Debug, Default)]
pub struct XLayerEvmFactory;

impl EvmFactory for XLayerEvmFactory {
    type Evm<DB: Database, I: Inspector<OpContext<DB>>> = OpEvm<DB, I, Self::Precompiles>;
    type Context<DB: Database> = OpContext<DB>;
    type Tx = OpTransaction<TxEnv>;
    type Error<DBError: core::error::Error + Send + Sync + 'static> =
        EVMError<DBError, OpTransactionError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let mut op_evm = OpEvmFactory::default().create_evm(db, input);
        let spec_id = op_evm.ctx().cfg.spec;
        *op_evm.components_mut().2 = xlayer_precompiles_map(spec_id);

        tracing::debug!(
            target: "xlayer::evm",
            spec = ?spec_id,
            "Injected XLayer precompiles into EVM"
        );

        op_evm
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let mut op_evm = OpEvmFactory::default().create_evm_with_inspector(db, input, inspector);
        let spec_id = op_evm.ctx().cfg.spec;
        *op_evm.components_mut().2 = xlayer_precompiles_map(spec_id);

        tracing::debug!(
            target: "xlayer::evm",
            spec = ?spec_id,
            "Injected XLayer precompiles into EVM (with inspector)"
        );

        op_evm
    }
}
