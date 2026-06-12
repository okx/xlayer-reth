//! FR-3 — included-as-reverted state surgery for an intercepted deposit.
//!
//! Mirrors op-revm's natural failed-deposit post-state (`op-revm` handler `catch_error`,
//! handler.rs:395-428) and op-geth's manual revert (`blacklist_gate_xlayer.go:255-288`):
//! discard ALL of the deposit's execution effects, then re-apply only the two effects a
//! failed deposit always keeps — bump the sender nonce to `N+1` and re-credit the mint.
//! Every other touched account reverts to its pre-tx committed value (by not being part of
//! the committed delta).
//!
//! Returns the minimal committed [`EvmState`] delta (sender account only). Shared by the
//! builder face (Step 3) and the follower executor face (Step 4) so both produce a
//! byte-identical post-state.

use alloy_primitives::Address;
use revm::state::{Account, AccountInfo, EvmState};
use xlayer_blacklist::deposit::RevertedDepositOutcome;

/// Build the single-account committed state delta for an intercepted (included-as-reverted)
/// deposit: the sender's committed pre-tx account info with `nonce = N+1` and `balance +=
/// mint` (mint kept). `pre_info` MUST be the sender's committed account info read before
/// this tx's state is committed (so `balance` is the pre-tx balance, not the reverted
/// execution's). All other accounts touched by the deposit are intentionally absent → they
/// revert to their pre-tx state.
pub fn reverted_deposit_state(
    sender: Address,
    pre_info: AccountInfo,
    outcome: &RevertedDepositOutcome,
) -> EvmState {
    let mut info = pre_info;
    info.nonce = outcome.account_nonce;
    if let Some(mint) = outcome.keep_mint {
        info.balance = info.balance.saturating_add(mint);
    }
    let mut account = Account { info, ..Default::default() };
    account.mark_touch();

    let mut state = EvmState::default();
    state.insert(sender, account);
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, U256};
    use xlayer_blacklist::deposit::apply_included_as_reverted;

    const SENDER: Address = address!("00000000000000000000000000000000000000aa");

    #[test]
    fn keeps_mint_and_bumps_nonce() {
        // pre: nonce 7, balance 100; deposit carried mint 50, Canyon active.
        let pre = AccountInfo { balance: U256::from(100u64), nonce: 7, ..Default::default() };
        let outcome = apply_included_as_reverted(7, 21_000, Some(U256::from(50u64)), true);
        let state = reverted_deposit_state(SENDER, pre, &outcome);
        let acct = state.get(&SENDER).expect("sender present");
        assert_eq!(acct.info.nonce, 8); // N+1
        assert_eq!(acct.info.balance, U256::from(150u64)); // pre + mint
        assert!(acct.is_touched());
    }

    #[test]
    fn no_mint_only_bumps_nonce() {
        let pre = AccountInfo { balance: U256::from(100u64), nonce: 0, ..Default::default() };
        let outcome = apply_included_as_reverted(0, 50_000, None, false);
        let state = reverted_deposit_state(SENDER, pre, &outcome);
        let acct = state.get(&SENDER).expect("sender present");
        assert_eq!(acct.info.nonce, 1);
        assert_eq!(acct.info.balance, U256::from(100u64)); // unchanged (no mint)
    }

    #[test]
    fn preserves_code_hash() {
        // a deposit sender that is a contract keeps its code hash through the revert.
        let code_hash = alloy_primitives::b256!(
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        let pre = AccountInfo { balance: U256::ZERO, nonce: 3, code_hash, ..Default::default() };
        let outcome = apply_included_as_reverted(3, 100, Some(U256::from(9u64)), true);
        let state = reverted_deposit_state(SENDER, pre, &outcome);
        assert_eq!(state.get(&SENDER).unwrap().info.code_hash, code_hash);
    }
}
