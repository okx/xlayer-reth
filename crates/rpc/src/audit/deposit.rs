//! Constructs an executable `Recovered<OpTransactionSigned>` deposit transaction from
//! already-decoded fields (the caller — RCS's L1 Listener — has already parsed the raw L1
//! `TransactionDeposited` log; this module does zero log-decoding).

use alloy_consensus::transaction::Recovered;
use alloy_consensus::Sealable;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use reth_optimism_primitives::OpTransactionSigned;
use serde::Deserialize;

/// One deposit transaction's already-decoded fields, as sent by the RCS L1 Listener.
#[derive(Debug, Clone, Deserialize)]
pub struct DepositTxRequest {
    pub source_hash: String,
    pub from: String,
    pub to: Option<String>,
    /// Decimal string; must fit in `u128` (see `DepositBuildError::MintOverflowsU128`).
    pub mint: String,
    /// Decimal string; parsed into `U256`.
    pub value: String,
    pub gas_limit: u64,
    pub is_system_transaction: bool,
    pub data: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DepositBuildError {
    #[error("invalid source_hash: {0}")]
    InvalidSourceHash(String),
    #[error("invalid from address: {0}")]
    InvalidFrom(String),
    #[error("invalid to address: {0}")]
    InvalidTo(String),
    #[error("mint value overflows u128: {0}")]
    MintOverflowsU128(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
    #[error("invalid data hex: {0}")]
    InvalidData(String),
}

/// Builds a `Recovered<OpTransactionSigned>` (the `Deposit` variant) directly from
/// already-decoded fields — no log parsing, no signature recovery (deposit txs are unsigned;
/// the "recovered signer" is the `from` field itself).
pub fn build_deposit_tx(
    req: &DepositTxRequest,
) -> Result<Recovered<OpTransactionSigned>, DepositBuildError> {
    let source_hash = req
        .source_hash
        .parse::<B256>()
        .map_err(|_| DepositBuildError::InvalidSourceHash(req.source_hash.clone()))?;
    let from = req
        .from
        .parse::<Address>()
        .map_err(|_| DepositBuildError::InvalidFrom(req.from.clone()))?;
    let to = match &req.to {
        Some(addr) => TxKind::Call(
            addr.parse::<Address>().map_err(|_| DepositBuildError::InvalidTo(addr.clone()))?,
        ),
        None => TxKind::Create,
    };
    let mint = req
        .mint
        .parse::<u128>()
        .map_err(|_| DepositBuildError::MintOverflowsU128(req.mint.clone()))?;
    let value = req
        .value
        .parse::<U256>()
        .map_err(|_| DepositBuildError::InvalidValue(req.value.clone()))?;
    let data =
        hex_decode(&req.data).map_err(|_| DepositBuildError::InvalidData(req.data.clone()))?;

    let deposit = TxDeposit {
        source_hash,
        from,
        to,
        mint,
        value,
        gas_limit: req.gas_limit,
        is_system_transaction: req.is_system_transaction,
        input: Bytes::from(data),
    };
    let envelope = OpTxEnvelope::Deposit(deposit.seal_slow());
    Ok(Recovered::new_unchecked(OpTransactionSigned::from(envelope), from))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, alloy_primitives::hex::FromHexError> {
    alloy_primitives::hex::decode(s.strip_prefix("0x").unwrap_or(s))
}

/// Test-only fixture shared with `verdict.rs`'s tests, so they reuse the same sample request
/// instead of duplicating its field values.
#[cfg(test)]
pub(super) fn tests_support_sample_request() -> DepositTxRequest {
    tests::valid_request()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn valid_request() -> DepositTxRequest {
        DepositTxRequest {
            source_hash: "0x".to_string() + &"11".repeat(32),
            from: "0x0404040404040404040404040404040404040404".to_string(),
            to: Some("0x0303030303030303030303030303030303030303".to_string()),
            mint: "0".to_string(),
            value: "1000000000000000000".to_string(),
            gas_limit: 100_000,
            is_system_transaction: false,
            data: "0x".to_string(),
        }
    }

    #[test]
    fn to_null_maps_to_create() {
        let mut req = valid_request();
        req.to = None;
        let tx = build_deposit_tx(&req).expect("valid request");
        assert!(matches!(tx.inner(), OpTxEnvelope::Deposit(dep) if dep.to.is_create()));
    }

    #[test]
    fn mint_overflowing_u128_is_rejected() {
        let mut req = valid_request();
        // u128::MAX + 1, as a decimal string.
        req.mint = "340282366920938463463374607431768211456".to_string();
        let err = build_deposit_tx(&req).expect_err("must reject mint overflow");
        assert!(matches!(err, DepositBuildError::MintOverflowsU128(_)));
    }

    #[test]
    fn mint_zero_is_accepted() {
        let req = valid_request();
        let tx = build_deposit_tx(&req).expect("mint=0 is valid");
        assert!(matches!(tx.inner(), OpTxEnvelope::Deposit(dep) if dep.mint == 0));
    }

    #[test]
    fn invalid_source_hash_is_rejected() {
        let mut req = valid_request();
        req.source_hash = "not-a-hash".to_string();
        let err = build_deposit_tx(&req).expect_err("must reject invalid source_hash");
        assert!(matches!(err, DepositBuildError::InvalidSourceHash(_)));
    }

    #[test]
    fn invalid_from_is_rejected() {
        let mut req = valid_request();
        req.from = "not-an-address".to_string();
        let err = build_deposit_tx(&req).expect_err("must reject invalid from address");
        assert!(matches!(err, DepositBuildError::InvalidFrom(_)));
    }

    #[test]
    fn invalid_to_is_rejected() {
        let mut req = valid_request();
        req.to = Some("not-an-address".to_string());
        let err = build_deposit_tx(&req).expect_err("must reject invalid to address");
        assert!(matches!(err, DepositBuildError::InvalidTo(_)));
    }

    #[test]
    fn invalid_value_is_rejected() {
        let mut req = valid_request();
        req.value = "not-a-number".to_string();
        let err = build_deposit_tx(&req).expect_err("must reject invalid value");
        assert!(matches!(err, DepositBuildError::InvalidValue(_)));
    }

    #[test]
    fn invalid_data_is_rejected() {
        let mut req = valid_request();
        req.data = "not-hex-zz".to_string();
        let err = build_deposit_tx(&req).expect_err("must reject invalid data hex");
        assert!(matches!(err, DepositBuildError::InvalidData(_)));
    }

    #[test]
    fn all_fields_survive_into_recovered_tx() {
        let req = DepositTxRequest {
            source_hash: "0x".to_string() + &"22".repeat(32),
            from: "0x0505050505050505050505050505050505050505".to_string(),
            to: Some("0x0606060606060606060606060606060606060606".to_string()),
            mint: "12345".to_string(),
            value: "987654321000000000".to_string(),
            gas_limit: 424_242,
            is_system_transaction: true,
            data: "0xdeadbeef".to_string(),
        };

        let expected_from = req.from.parse::<Address>().expect("valid from address");
        let expected_to =
            req.to.as_deref().expect("to is set").parse::<Address>().expect("valid to address");
        let expected_source_hash = req.source_hash.parse::<B256>().expect("valid source_hash");
        let expected_value = req.value.parse::<U256>().expect("valid value");

        let tx = build_deposit_tx(&req).expect("all fields are valid");

        match tx.inner() {
            OpTxEnvelope::Deposit(dep) => {
                assert_eq!(dep.source_hash, expected_source_hash);
                assert_eq!(dep.from, expected_from);
                assert_eq!(dep.to, TxKind::Call(expected_to));
                assert_eq!(dep.mint, 12345u128);
                assert_eq!(dep.value, expected_value);
                assert_eq!(dep.gas_limit, 424_242);
                assert!(dep.is_system_transaction);
                assert_eq!(dep.input, Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
            }
            other => panic!("expected Deposit variant, got {other:?}"),
        }

        assert_eq!(tx.signer(), expected_from);
    }
}
