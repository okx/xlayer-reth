//! FR-3 — L1 forced-inclusion deposit handling: included-as-reverted.
//!
//! A deposit touching a blacklisted address cannot be dropped (OP protocol forces
//! inclusion), so it is included but reverted: state rolled back, mint kept, sender nonce
//! `N+1`, `status=0`, `gasUsed = gasLimit`, `DepositNonce = N` (pre-exec), empty logs.
//! Both the builder and the follower/executor faces apply the identical disposition.
//!
//! This module computes the **field overrides** as pure data; the executor adapter
//! applies them to the revm/reth receipt + state. [TD §4.4]

use alloy_primitives::{address, Address, U256};

/// System call sender (`0xff..fe`) — EIP-4788 / EIP-2935 / withdrawals. Never intercepted.
pub const SYSTEM_ADDRESS: Address = address!("fffffffffffffffffffffffffffffffffffffffe");
/// L1-attributes depositor (`0xDeaD…0001`) — per-block first L1-info deposit. Never intercepted.
pub const L1_ATTRIBUTES_DEPOSITOR: Address = address!("deaddeaddeaddeaddeaddeaddeaddeaddead0001");

/// Exhaustive exempt-sender set: deposits from these addresses are never intercepted,
/// even if they touch a blacklisted address. [TD §4.4]
pub const EXEMPT_SENDERS: [Address; 2] = [SYSTEM_ADDRESS, L1_ATTRIBUTES_DEPOSITOR];

/// `CanyonDepositReceiptVersion` — written to intercepted-deposit receipts once Canyon is
/// active (matches op-geth). [TD §4.4]
pub const CANYON_DEPOSIT_RECEIPT_VERSION: u64 = 1;

/// Whether a deposit from `from` is exempt from interception.
pub fn is_exempt_sender(from: &Address) -> bool {
    EXEMPT_SENDERS.contains(from)
}

/// The receipt + state field overrides applied to an intercepted deposit. Every field is
/// a cross-client constant rule; any divergence forks the receipts root. [TD §4.4, DM-3.x]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevertedDepositOutcome {
    /// Receipt status — always `false` (0).
    pub status: bool,
    /// Receipt `gasUsed` — full `gasLimit` (explicit override, not natural consumption).
    pub gas_used: u64,
    /// `receipt.DepositNonce` — the pre-exec sender nonce `N` (NOT the post-state `N+1`).
    pub deposit_nonce: u64,
    /// Post-state sender account nonce — `N+1` (natural-failure semantics).
    pub account_nonce: u64,
    /// `receipt.DepositReceiptVersion` — `Some(CANYON…)` iff Canyon active, else `None`.
    pub deposit_receipt_version: Option<u64>,
    /// Mint amount to re-credit after rollback (`Some` iff the deposit carried a mint).
    pub keep_mint: Option<U256>,
    /// Whether to clear the receipt logs (always `true` — logs become `∅`, bloom recomputed).
    pub clear_logs: bool,
}

/// Compute the included-as-reverted field overrides for an intercepted deposit.
///
/// - `pre_exec_nonce` — sender nonce `N` captured before execution.
/// - `gas_limit`      — the deposit's gas limit (becomes `gasUsed`).
/// - `mint`           — `Some(amount)` if the deposit carries a mint, else `None`.
/// - `canyon_active`  — whether Canyon is active at the block timestamp.
pub fn apply_included_as_reverted(
    pre_exec_nonce: u64,
    gas_limit: u64,
    mint: Option<U256>,
    canyon_active: bool,
) -> RevertedDepositOutcome {
    RevertedDepositOutcome {
        status: false,
        gas_used: gas_limit,
        deposit_nonce: pre_exec_nonce,
        account_nonce: pre_exec_nonce.saturating_add(1),
        deposit_receipt_version: canyon_active.then_some(CANYON_DEPOSIT_RECEIPT_VERSION),
        keep_mint: mint,
        clear_logs: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_and_l1_attributes_are_exempt() {
        // DM-3.11 / DM-2.16
        assert!(is_exempt_sender(&SYSTEM_ADDRESS));
        assert!(is_exempt_sender(&L1_ATTRIBUTES_DEPOSITOR));
    }

    #[test]
    fn normal_sender_is_not_exempt() {
        assert!(!is_exempt_sender(&address!("00000000000000000000000000000000000000aa")));
    }

    #[test]
    fn reverted_deposit_field_rules_canyon_active() {
        // DM-3.1..3.9 (Canyon active)
        let o = apply_included_as_reverted(7, 21_000, Some(U256::from(1_000u64)), true);
        assert!(!o.status);
        assert_eq!(o.gas_used, 21_000);
        assert_eq!(o.deposit_nonce, 7); // N
        assert_eq!(o.account_nonce, 8); // N+1
        assert_eq!(o.deposit_receipt_version, Some(CANYON_DEPOSIT_RECEIPT_VERSION));
        assert_eq!(o.keep_mint, Some(U256::from(1_000u64)));
        assert!(o.clear_logs);
    }

    #[test]
    fn reverted_deposit_no_version_when_canyon_inactive() {
        // DM-3.8
        let o = apply_included_as_reverted(0, 50_000, None, false);
        assert_eq!(o.deposit_receipt_version, None);
        assert_eq!(o.keep_mint, None);
        assert_eq!(o.account_nonce, 1);
    }

    #[test]
    fn deposit_nonce_and_account_nonce_intentionally_differ() {
        // DM-3.6 — DepositNonce = N, account nonce = N+1.
        let o = apply_included_as_reverted(42, 100, None, true);
        assert_eq!(o.deposit_nonce, 42);
        assert_eq!(o.account_nonce, 43);
        assert_ne!(o.deposit_nonce, o.account_nonce);
    }
}
