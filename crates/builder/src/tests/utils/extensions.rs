use super::signers::{builder_signer, flashblocks_number_signer};
use crate::{
    tests::{flashblocks_number_contract::FlashblocksNumber, ChainDriver, Protocol},
    tx::signer::Signer,
};
use alloy_eips::Encodable2718;
use alloy_primitives::{hex, Address, BlockHash, TxHash, TxKind, B256, U256};
use alloy_rpc_types_eth::Block;
use alloy_sol_types::SolCall;
use core::future::Future;
use op_alloy_consensus::{OpTypedTransaction, TxDeposit};
use op_alloy_rpc_types::Transaction;

use crate::tests::TransactionBuilder;

pub trait TransactionBuilderExt {
    fn random_valid_transfer(self) -> Self;
    fn random_reverting_transaction(self) -> Self;
    fn random_big_transaction(self) -> Self;
    // flashblocks number methods
    fn deploy_flashblock_number_contract(self) -> Self;
    fn init_flashblock_number_contract(self, register_builder: bool) -> Self;
    fn add_authorized_builder(self, builder: Address) -> Self;
}

impl TransactionBuilderExt for TransactionBuilder {
    fn random_valid_transfer(self) -> Self {
        self.with_to(rand::random::<Address>()).with_value(1)
    }

    fn random_reverting_transaction(self) -> Self {
        self.with_create().with_input(hex!("60006000fd").into()) // PUSH1 0x00 PUSH1 0x00 REVERT
    }

    // This transaction is big in the sense that it uses a lot of gas. The exact
    // amount it uses is 86220 gas.
    fn random_big_transaction(self) -> Self {
        // PUSH13 0x63ffffffff60005260046000f3 PUSH1 0x00 MSTORE PUSH1 0x02 PUSH1 0x0d PUSH1 0x13 PUSH1 0x00 CREATE2
        self.with_create()
            .with_input(hex!("6c63ffffffff60005260046000f36000526002600d60136000f5").into())
    }

    fn deploy_flashblock_number_contract(self) -> Self {
        self.with_create()
            .with_input(FlashblocksNumber::BYTECODE.clone())
            .with_gas_limit(2_000_000) // deployment costs ~1.6 million gas
            .with_signer(flashblocks_number_signer())
    }

    fn init_flashblock_number_contract(self, register_builder: bool) -> Self {
        let builder_signer = builder_signer();
        let owner = flashblocks_number_signer();

        let init_data = FlashblocksNumber::initializeCall {
            _owner: owner.address,
            _initialBuilders: if register_builder { vec![builder_signer.address] } else { vec![] },
        }
        .abi_encode();

        self.with_input(init_data.into()).with_signer(flashblocks_number_signer())
    }

    fn add_authorized_builder(self, builder: Address) -> Self {
        let calldata = FlashblocksNumber::addBuilderCall { builder }.abi_encode();

        self.with_input(calldata.into()).with_signer(flashblocks_number_signer())
    }
}

pub trait ChainDriverExt {
    fn fund_many(
        &self,
        addresses: Vec<Address>,
        amount: u128,
    ) -> impl Future<Output = eyre::Result<BlockHash>>;
    fn fund(&self, address: Address, amount: u128)
        -> impl Future<Output = eyre::Result<BlockHash>>;

    fn fund_accounts(
        &self,
        count: usize,
        amount: u128,
    ) -> impl Future<Output = eyre::Result<Vec<Signer>>> {
        async move {
            let accounts = (0..count).map(|_| Signer::random()).collect::<Vec<_>>();
            self.fund_many(accounts.iter().map(|a| a.address).collect(), amount).await?;
            Ok(accounts)
        }
    }

    fn build_new_block_with_valid_transaction(
        &self,
    ) -> impl Future<Output = eyre::Result<(TxHash, Block<Transaction>)>>;

    fn build_new_block_with_reverting_transaction(
        &self,
    ) -> impl Future<Output = eyre::Result<(TxHash, Block<Transaction>)>>;
}

impl<P: Protocol> ChainDriverExt for ChainDriver<P> {
    async fn fund_many(&self, addresses: Vec<Address>, amount: u128) -> eyre::Result<BlockHash> {
        let mut txs = Vec::with_capacity(addresses.len());

        for address in addresses {
            let deposit = TxDeposit {
                source_hash: B256::default(),
                from: address, // Set the sender to the address of the account to seed
                to: TxKind::Create,
                mint: amount, // Amount to deposit
                value: U256::default(),
                gas_limit: 210000,
                is_system_transaction: false,
                input: Default::default(), // No input data for the deposit
            };

            let signer = Signer::random();
            let signed_tx = signer.sign_tx(OpTypedTransaction::Deposit(deposit))?;
            let signed_tx_rlp = signed_tx.encoded_2718();
            txs.push(signed_tx_rlp.into());
        }

        Ok(self.build_new_block_with_txs(txs).await?.header.hash)
    }

    async fn fund(&self, address: Address, amount: u128) -> eyre::Result<BlockHash> {
        let deposit = TxDeposit {
            source_hash: B256::default(),
            from: address, // Set the sender to the address of the account to seed
            to: TxKind::Create,
            mint: amount, // Amount to deposit
            value: U256::default(),
            gas_limit: 210000,
            is_system_transaction: false,
            input: Default::default(), // No input data for the deposit
        };

        let signer = Signer::random();
        let signed_tx = signer.sign_tx(OpTypedTransaction::Deposit(deposit))?;
        let signed_tx_rlp = signed_tx.encoded_2718();
        Ok(self.build_new_block_with_txs(vec![signed_tx_rlp.into()]).await?.header.hash)
    }

    async fn build_new_block_with_valid_transaction(
        &self,
    ) -> eyre::Result<(TxHash, Block<Transaction>)> {
        let tx = self.create_transaction().random_valid_transfer().send().await?;
        Ok((*tx.tx_hash(), self.build_new_block().await?))
    }

    async fn build_new_block_with_reverting_transaction(
        &self,
    ) -> eyre::Result<(TxHash, Block<Transaction>)> {
        let tx = self.create_transaction().random_reverting_transaction().send().await?;

        Ok((*tx.tx_hash(), self.build_new_block().await?))
    }
}

pub trait OpRbuilderArgsTestExt {
    fn test_default() -> Self;
}

impl OpRbuilderArgsTestExt for crate::args::OpRbuilderArgs {
    fn test_default() -> Self {
        let mut default = Self::default();
        default.flashblocks.flashblocks_port = 0; // randomize port
        default
    }
}
