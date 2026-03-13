use crate::{
    flashblocks::{context::FlashblocksBuilderCtx, utils::execution::ExecutionInfo},
    signer::Signer,
};
use core::fmt::Debug;
use tracing::{trace, warn};

use alloy_consensus::TxEip1559;
use alloy_eips::{eip7623::TOTAL_COST_FLOOR_PER_TOKEN, Encodable2718};
use alloy_evm::{rpc::TryIntoTxEnv, Database};
use alloy_op_evm::OpEvm;
use alloy_primitives::{map::HashSet, Address, Bytes, TxKind, B256};
use alloy_rpc_types_eth::TransactionInput;
use alloy_sol_types::{sol, ContractError, Revert, SolCall, SolError, SolEvent, SolInterface};
use op_alloy_consensus::OpTypedTransaction;
use op_alloy_rpc_types::OpTransactionRequest;
use op_revm::{OpHaltReason, OpTransactionError};
use revm::{
    context::result::{EVMError, ExecutionResult, ResultAndState},
    inspector::NoOpInspector,
    DatabaseCommit, DatabaseRef,
};

use reth_evm::{
    eth::receipt_builder::ReceiptBuilderCtx, precompiles::PrecompilesMap, ConfigureEvm, Evm,
    EvmError, InvalidTxError,
};
use reth_node_api::PayloadBuilderError;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::Recovered;
use reth_provider::{ProviderError, StateProvider};
use reth_revm::{database::StateProviderDatabase, State};
use reth_rpc_api::eth::EthTxEnvError;

sol!(
    // From https://github.com/Uniswap/flashblocks_number_contract/blob/main/src/FlashblockNumber.sol
    #[sol(rpc, abi)]
    #[derive(Debug)]
    interface IFlashblockNumber {
        uint256 public flashblockNumber;

        function incrementFlashblockNumber() external;

        function permitIncrementFlashblockNumber(uint256 currentFlashblockNumber, bytes memory signature) external;

        function computeStructHash(uint256 currentFlashblockNumber) external pure returns (bytes32);

        function hashTypedDataV4(bytes32 structHash) external view returns (bytes32);


        // @notice Emitted when flashblock index is incremented
        // @param newFlashblockIndex The new flashblock index (0-indexed within each L2 block)
        event FlashblockIncremented(uint256 newFlashblockIndex);

        /// -----------------------------------------------------------------------
        /// Errors
        /// -----------------------------------------------------------------------
        error NonBuilderAddress(address addr);
        error MismatchedFlashblockNumber(uint256 expectedFlashblockNumber, uint256 actualFlashblockNumber);
    }
);

#[derive(Debug, Clone)]
pub(crate) struct BuilderTransactionCtx {
    pub(crate) gas_used: u64,
    pub(crate) da_size: u64,
    pub(crate) signed_tx: Recovered<OpTransactionSigned>,
    // whether the transaction should be a top of block or
    // bottom of block transaction
    pub(crate) is_top_of_block: bool,
}

impl BuilderTransactionCtx {
    fn set_top_of_block(mut self) -> Self {
        self.is_top_of_block = true;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvalidContractDataError {
    #[error("did not find expected logs expected {0:?} but got {1:?}")]
    InvalidLogs(Vec<B256>, Vec<B256>),
    #[error("could not decode output from contract call")]
    OutputAbiDecodeError,
}

/// Possible error variants during construction of builder txs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BuilderTransactionError {
    /// Builder account load fails to get builder nonce
    #[error("failed to load account {0}")]
    AccountLoadFailed(Address),
    /// Signature signing fails
    #[error("failed to sign transaction: {0}")]
    SigningError(secp256k1::Error),
    /// Invalid contract errors indicating the contract is incorrect
    #[error("contract {0} may be incorrect, invalid contract data: {1}")]
    InvalidContract(Address, InvalidContractDataError),
    /// Transaction halted execution
    #[error("transaction to {0} halted {1:?}")]
    TransactionHalted(Address, OpHaltReason),
    /// Transaction reverted
    #[error("transaction to {0} reverted {1}")]
    TransactionReverted(Address, Revert),
    /// Invalid tx errors during evm execution.
    #[error("invalid transaction error {0}")]
    InvalidTransactionError(Box<dyn core::error::Error + Send + Sync>),
    /// Unrecoverable error during evm execution.
    #[error("evm execution error {0}")]
    EvmExecutionError(Box<dyn core::error::Error + Send + Sync>),
}

impl From<secp256k1::Error> for BuilderTransactionError {
    fn from(error: secp256k1::Error) -> Self {
        BuilderTransactionError::SigningError(error)
    }
}

impl From<EVMError<ProviderError, OpTransactionError>> for BuilderTransactionError {
    fn from(error: EVMError<ProviderError, OpTransactionError>) -> Self {
        BuilderTransactionError::EvmExecutionError(Box::new(error))
    }
}

impl From<EthTxEnvError> for BuilderTransactionError {
    fn from(error: EthTxEnvError) -> Self {
        BuilderTransactionError::EvmExecutionError(Box::new(error))
    }
}

impl From<BuilderTransactionError> for PayloadBuilderError {
    fn from(error: BuilderTransactionError) -> Self {
        match error {
            BuilderTransactionError::EvmExecutionError(e) => {
                PayloadBuilderError::EvmExecutionError(e)
            }
            _ => PayloadBuilderError::other(error),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum FlashblocksBuilderTx {
    /// Simple builder tx using only `BuilderTxBase`
    Base(BuilderTxBase),
    /// Builder tx with flashblock number contract integration
    NumberContract { signer: Signer, flashblock_number_address: Address, base: BuilderTxBase },
}

impl FlashblocksBuilderTx {
    /// Creates a new base-only builder tx.
    pub(crate) fn new_base(signer: Option<Signer>) -> Self {
        Self::Base(BuilderTxBase::new(signer))
    }

    /// Creates a new builder tx with flashblock number contract integration.
    pub(crate) fn new_number_contract(signer: Signer, flashblock_number_address: Address) -> Self {
        Self::NumberContract {
            signer,
            flashblock_number_address,
            base: BuilderTxBase::new(Some(signer)),
        }
    }

    // Simulates and returns the signed builder transactions. The simulation modifies and commits
    // changes to the db so call `new_simulation_state` to simulate on a new copy of the state.
    fn simulate_builder_txs(
        &self,
        ctx: &FlashblocksBuilderCtx,
        db: &mut State<impl Database + DatabaseRef>,
        is_first_flashblock: bool,
        is_last_flashblock: bool,
    ) -> Result<Vec<BuilderTransactionCtx>, BuilderTransactionError> {
        match self {
            Self::Base(base) => Self::simulate_base_builder_txs(
                base,
                ctx,
                db,
                is_first_flashblock,
                is_last_flashblock,
            ),
            Self::NumberContract { signer, flashblock_number_address, base } => {
                Self::simulate_number_contract_builder_txs(
                    *signer,
                    *flashblock_number_address,
                    base,
                    ctx,
                    db,
                    is_first_flashblock,
                )
            }
        }
    }

    fn simulate_base_builder_txs(
        base: &BuilderTxBase,
        ctx: &FlashblocksBuilderCtx,
        db: &mut State<impl Database + DatabaseRef>,
        is_first_flashblock: bool,
        is_last_flashblock: bool,
    ) -> Result<Vec<BuilderTransactionCtx>, BuilderTransactionError> {
        let mut builder_txs = Vec::<BuilderTransactionCtx>::new();

        if is_first_flashblock {
            let flashblocks_builder_tx = base.simulate_builder_tx(ctx, &mut *db)?;
            builder_txs.extend(flashblocks_builder_tx.clone());
        }

        if is_last_flashblock {
            let base_tx = base.simulate_builder_tx(ctx, &mut *db)?;
            builder_txs.extend(base_tx);
        }
        Ok(builder_txs)
    }

    fn simulate_number_contract_builder_txs(
        signer: Signer,
        flashblock_number_address: Address,
        base: &BuilderTxBase,
        ctx: &FlashblocksBuilderCtx,
        db: &mut State<impl Database + DatabaseRef>,
        is_first_flashblock: bool,
    ) -> Result<Vec<BuilderTransactionCtx>, BuilderTransactionError> {
        let mut builder_txs = Vec::<BuilderTransactionCtx>::new();

        if is_first_flashblock {
            // fallback block builder tx
            builder_txs.extend(base.simulate_builder_tx(ctx, &mut *db)?);
        } else {
            // we increment the flashblock number for the next flashblock so we don't increment in the last flashblock
            let mut evm = ctx.evm_config.evm_with_env(&mut *db, ctx.evm_env.clone());
            evm.modify_cfg(|cfg| {
                cfg.disable_balance_check = true;
                cfg.disable_block_gas_limit = true;
            });

            let flashblocks_num_tx = Self::signed_increment_flashblocks_tx(
                signer,
                flashblock_number_address,
                ctx,
                &mut evm,
            );

            let tx = match flashblocks_num_tx {
                Ok(tx) => Some(tx),
                Err(e) => {
                    warn!(target: "builder_tx", error = ?e, "flashblocks number contract tx simulation failed, defaulting to fallback builder tx");
                    base.simulate_builder_tx(ctx, &mut *db)?.map(|tx| tx.set_top_of_block())
                }
            };

            builder_txs.extend(tx);
        }

        Ok(builder_txs)
    }

    fn signed_increment_flashblocks_tx(
        signer: Signer,
        flashblock_number_address: Address,
        ctx: &FlashblocksBuilderCtx,
        evm: &mut OpEvm<impl Database + DatabaseRef, NoOpInspector, PrecompilesMap>,
    ) -> Result<BuilderTransactionCtx, BuilderTransactionError> {
        let calldata = IFlashblockNumber::incrementFlashblockNumberCall {};
        Self::increment_flashblocks_tx(signer, flashblock_number_address, calldata, ctx, evm)
    }

    fn increment_flashblocks_tx<T: SolCall + Clone>(
        signer: Signer,
        flashblock_number_address: Address,
        calldata: T,
        ctx: &FlashblocksBuilderCtx,
        evm: &mut OpEvm<impl Database + DatabaseRef, NoOpInspector, PrecompilesMap>,
    ) -> Result<BuilderTransactionCtx, BuilderTransactionError> {
        let gas_used = Self::simulate_flashblocks_call(
            signer,
            flashblock_number_address,
            calldata.clone(),
            vec![IFlashblockNumber::FlashblockIncremented::SIGNATURE_HASH],
            ctx,
            evm,
        )?;
        let signed_tx = Self::sign_tx(
            flashblock_number_address,
            signer,
            gas_used,
            calldata.abi_encode().into(),
            ctx,
            evm.db_mut(),
        )?;
        let da_size =
            op_alloy_flz::tx_estimated_size_fjord_bytes(signed_tx.encoded_2718().as_slice());
        Ok(BuilderTransactionCtx { signed_tx, gas_used, da_size, is_top_of_block: true })
    }

    fn simulate_flashblocks_call<T: SolCall>(
        signer: Signer,
        flashblock_number_address: Address,
        calldata: T,
        expected_logs: Vec<B256>,
        ctx: &FlashblocksBuilderCtx,
        evm: &mut OpEvm<impl Database + DatabaseRef, NoOpInspector, PrecompilesMap>,
    ) -> Result<u64, BuilderTransactionError> {
        let tx_req = OpTransactionRequest::default()
            .gas_limit(ctx.block_gas_limit())
            .max_fee_per_gas(ctx.base_fee().into())
            .to(flashblock_number_address)
            .from(signer.address) // use tee key as signer for simulations
            .nonce(get_nonce(evm.db(), signer.address)?)
            .input(TransactionInput::new(calldata.abi_encode().into()));
        Self::simulate_call::<T, IFlashblockNumber::IFlashblockNumberErrors>(
            tx_req,
            expected_logs,
            evm,
        )
    }

    fn simulate_builder_txs_with_state_copy(
        &self,
        state_provider: impl StateProvider + Clone,
        ctx: &FlashblocksBuilderCtx,
        db: &State<impl Database>,
        is_first_flashblock: bool,
        is_last_flashblock: bool,
    ) -> Result<Vec<BuilderTransactionCtx>, BuilderTransactionError> {
        let mut simulation_state = Self::new_simulation_state(state_provider, db);
        self.simulate_builder_txs(
            ctx,
            &mut simulation_state,
            is_first_flashblock,
            is_last_flashblock,
        )
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) fn add_builder_txs(
        &self,
        state_provider: impl StateProvider + Clone,
        info: &mut ExecutionInfo,
        builder_ctx: &FlashblocksBuilderCtx,
        db: &mut State<impl Database>,
        top_of_block: bool,
        is_first_flashblock: bool,
        is_last_flashblock: bool,
    ) -> Result<Vec<BuilderTransactionCtx>, BuilderTransactionError> {
        let builder_txs = self.simulate_builder_txs_with_state_copy(
            state_provider,
            builder_ctx,
            db,
            is_first_flashblock,
            is_last_flashblock,
        )?;

        let mut evm = builder_ctx.evm_config.evm_with_env(&mut *db, builder_ctx.evm_env.clone());

        let mut invalid = HashSet::new();

        for builder_tx in builder_txs.iter() {
            if builder_tx.is_top_of_block != top_of_block {
                // don't commit tx if the buidler tx is not being added in the intended
                // position in the block
                continue;
            }
            if invalid.contains(&builder_tx.signed_tx.signer()) {
                warn!(target: "payload_builder", tx_hash = ?builder_tx.signed_tx.tx_hash(), "builder signer invalid as previous builder tx reverted");
                continue;
            }

            let ResultAndState { result, state } = match evm.transact(&builder_tx.signed_tx) {
                Ok(res) => res,
                Err(err) => {
                    if let Some(err) = err.as_invalid_tx_err() {
                        if err.is_nonce_too_low() {
                            // if the nonce is too low, we can skip this transaction
                            trace!(target: "payload_builder", %err, ?builder_tx.signed_tx, "skipping nonce too low builder transaction");
                        } else {
                            // if the transaction is invalid, we can skip it and all of its
                            // descendants
                            trace!(target: "payload_builder", %err, ?builder_tx.signed_tx, "skipping invalid builder transaction and its descendants");
                            invalid.insert(builder_tx.signed_tx.signer());
                        }

                        continue;
                    }
                    // this is an error that we should treat as fatal for this attempt
                    return Err(BuilderTransactionError::EvmExecutionError(Box::new(err)));
                }
            };

            if !result.is_success() {
                warn!(target: "payload_builder", tx_hash = ?builder_tx.signed_tx.tx_hash(), result = ?result, "builder tx reverted");
                invalid.insert(builder_tx.signed_tx.signer());
                continue;
            }

            // Add gas used by the transaction to cumulative gas used, before creating the receipt
            let gas_used = result.gas_used();
            info.cumulative_gas_used += gas_used;
            info.cumulative_da_bytes_used += builder_tx.da_size;

            let ctx = ReceiptBuilderCtx {
                tx_type: builder_tx.signed_tx.inner().tx_type(),
                evm: &evm,
                result,
                state: &state,
                cumulative_gas_used: info.cumulative_gas_used,
            };
            info.receipts.push(builder_ctx.build_receipt(ctx, None));

            // Commit changes
            evm.db_mut().commit(state);

            // Append sender and transaction to the respective lists
            info.executed_senders.push(builder_tx.signed_tx.signer());
            info.executed_transactions.push(builder_tx.signed_tx.clone().into_inner());
        }

        // Release the db reference by dropping evm
        drop(evm);

        Ok(builder_txs)
    }

    // Creates a copy of the state to simulate against
    fn new_simulation_state(
        state_provider: impl StateProvider,
        db: &State<impl Database>,
    ) -> State<StateProviderDatabase<impl StateProvider>> {
        let state = StateProviderDatabase::new(state_provider);

        State::builder()
            .with_database(state)
            .with_cached_prestate(db.cache.clone())
            .with_bundle_update()
            .build()
    }

    fn sign_tx(
        to: Address,
        from: Signer,
        gas_used: u64,
        calldata: Bytes,
        ctx: &FlashblocksBuilderCtx,
        db: impl DatabaseRef,
    ) -> Result<Recovered<OpTransactionSigned>, BuilderTransactionError> {
        let nonce = get_nonce(db, from.address)?;
        // Create the EIP-1559 transaction
        let tx = OpTypedTransaction::Eip1559(TxEip1559 {
            chain_id: ctx.chain_id(),
            nonce,
            // Due to EIP-150, 63/64 of available gas is forwarded to external calls so need to add a buffer
            gas_limit: gas_used * 64 / 63,
            max_fee_per_gas: ctx.base_fee().into(),
            to: TxKind::Call(to),
            input: calldata,
            ..Default::default()
        });
        Ok(from.sign_tx(tx)?)
    }

    fn simulate_call<T: SolCall, E: SolInterface + Debug>(
        tx: OpTransactionRequest,
        expected_logs: Vec<B256>,
        evm: &mut OpEvm<impl Database, NoOpInspector, PrecompilesMap>,
    ) -> Result<u64, BuilderTransactionError> {
        let evm_env = alloy_evm::EvmEnv::from((evm.cfg.clone(), evm.block.clone()));
        let tx_env = tx.try_into_tx_env(&evm_env)?;
        let to = tx_env.base.kind.into_to().unwrap_or_default();

        let ResultAndState { result, .. } = match evm.transact(tx_env) {
            Ok(res) => res,
            Err(err) => {
                if err.is_invalid_tx_err() {
                    return Err(BuilderTransactionError::InvalidTransactionError(Box::new(err)));
                } else {
                    return Err(BuilderTransactionError::EvmExecutionError(Box::new(err)));
                }
            }
        };

        match result {
            ExecutionResult::Success { output, gas_used, logs, .. } => {
                let topics: HashSet<B256> =
                    logs.into_iter().flat_map(|log| log.topics().to_vec()).collect();
                if !expected_logs.iter().all(|expected_topic| topics.contains(expected_topic)) {
                    return Err(BuilderTransactionError::InvalidContract(
                        to,
                        InvalidContractDataError::InvalidLogs(
                            expected_logs,
                            topics.into_iter().collect(),
                        ),
                    ));
                }
                let _ = T::abi_decode_returns(&output.into_data()).map_err(|_| {
                    BuilderTransactionError::InvalidContract(
                        to,
                        InvalidContractDataError::OutputAbiDecodeError,
                    )
                })?;
                Ok(gas_used)
            }
            ExecutionResult::Revert { output, .. } => {
                let revert = ContractError::<E>::abi_decode(&output)
                    .map(|reason| Revert::from(format!("{reason:?}")))
                    .or_else(|_| Revert::abi_decode(&output))
                    .unwrap_or_else(|_| {
                        Revert::from(format!("unknown revert: {}", hex::encode(&output)))
                    });
                Err(BuilderTransactionError::TransactionReverted(to, revert))
            }
            ExecutionResult::Halt { reason, .. } => {
                Err(BuilderTransactionError::TransactionHalted(to, reason))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BuilderTxBase {
    signer: Option<Signer>,
}

impl BuilderTxBase {
    fn new(signer: Option<Signer>) -> Self {
        Self { signer }
    }

    fn simulate_builder_tx(
        &self,
        ctx: &FlashblocksBuilderCtx,
        db: impl DatabaseRef,
    ) -> Result<Option<BuilderTransactionCtx>, BuilderTransactionError> {
        match self.signer {
            Some(signer) => {
                let message: Vec<u8> = format!("Block Number: {}", ctx.block_number()).into_bytes();
                let gas_used = self.estimate_builder_tx_gas(&message);
                let signed_tx = self.signed_builder_tx(ctx, db, signer, gas_used, message)?;
                let da_size = op_alloy_flz::tx_estimated_size_fjord_bytes(
                    signed_tx.encoded_2718().as_slice(),
                );
                Ok(Some(BuilderTransactionCtx {
                    gas_used,
                    da_size,
                    signed_tx,
                    is_top_of_block: false,
                }))
            }
            None => Ok(None),
        }
    }

    fn estimate_builder_tx_gas(&self, input: &[u8]) -> u64 {
        // Count zero and non-zero bytes
        let (zero_bytes, nonzero_bytes) = input.iter().fold((0, 0), |(zeros, nonzeros), &byte| {
            if byte == 0 {
                (zeros + 1, nonzeros)
            } else {
                (zeros, nonzeros + 1)
            }
        });

        // Calculate gas cost (4 gas per zero byte, 16 gas per non-zero byte)
        let zero_cost = zero_bytes * 4;
        let nonzero_cost = nonzero_bytes * 16;

        // Tx gas should be not less than floor gas https://eips.ethereum.org/EIPS/eip-7623
        let tokens_in_calldata = zero_bytes + nonzero_bytes * 4;
        let floor_gas = 21_000 + tokens_in_calldata * TOTAL_COST_FLOOR_PER_TOKEN;

        std::cmp::max(zero_cost + nonzero_cost + 21_000, floor_gas)
    }

    fn signed_builder_tx(
        &self,
        ctx: &FlashblocksBuilderCtx,
        db: impl DatabaseRef,
        signer: Signer,
        gas_used: u64,
        message: Vec<u8>,
    ) -> Result<Recovered<OpTransactionSigned>, BuilderTransactionError> {
        let nonce = get_nonce(db, signer.address)?;

        // Create the EIP-1559 transaction
        let tx = OpTypedTransaction::Eip1559(TxEip1559 {
            chain_id: ctx.chain_id(),
            nonce,
            gas_limit: gas_used,
            max_fee_per_gas: ctx.base_fee().into(),
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(Address::ZERO),
            // Include the message as part of the transaction data
            input: message.into(),
            ..Default::default()
        });
        // Sign the transaction
        let builder_tx = signer.sign_tx(tx).map_err(BuilderTransactionError::SigningError)?;

        Ok(builder_tx)
    }
}

pub(crate) fn get_nonce(
    db: impl DatabaseRef,
    address: Address,
) -> Result<u64, BuilderTransactionError> {
    db.basic_ref(address)
        .map(|acc| acc.unwrap_or_default().nonce)
        .map_err(|_| BuilderTransactionError::AccountLoadFailed(address))
}
