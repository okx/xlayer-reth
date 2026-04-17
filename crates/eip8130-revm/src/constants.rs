//! Compile-time constant verification for EIP-8130.
//!
//! Asserts that the constants used by the execution engine match
//! those defined in `xlayer-eip8130-consensus`. If the consensus
//! crate changes a constant, a compile error will surface here.

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use xlayer_eip8130_consensus::*;

    // ── Transaction type ──────────────────────────────────────────────

    #[test]
    fn aa_tx_type_id_is_0x7b() {
        assert_eq!(AA_TX_TYPE_ID, 0x7B);
    }

    // ── Predeploy addresses ───────────────────────────────────────────

    #[test]
    fn nonce_manager_address() {
        let expected: Address = "0x000000000000000000000000000000000000aa02".parse().unwrap();
        assert_eq!(NONCE_MANAGER_ADDRESS, expected);
    }

    #[test]
    fn tx_context_address() {
        let expected: Address = "0x000000000000000000000000000000000000aa03".parse().unwrap();
        assert_eq!(TX_CONTEXT_ADDRESS, expected);
    }

    #[test]
    fn account_config_address() {
        let expected: Address = "0xf946601D5424118A4e4054BB0B13133f216b4FeE".parse().unwrap();
        assert_eq!(ACCOUNT_CONFIG_ADDRESS, expected);
    }

    #[test]
    fn k1_verifier_is_ecrecover() {
        let expected: Address = "0x0000000000000000000000000000000000000001".parse().unwrap();
        assert_eq!(K1_VERIFIER_ADDRESS, expected);
    }

    // ── Gas constants ─────────────────────────────────────────────────

    #[test]
    fn aa_base_cost() {
        assert_eq!(AA_BASE_COST, 15_000);
    }

    #[test]
    fn nonce_key_gas_values() {
        assert_eq!(NONCE_KEY_COLD_GAS, 22_100);
        assert_eq!(NONCE_KEY_WARM_GAS, 5_000);
    }

    #[test]
    fn bytecode_gas_values() {
        assert_eq!(BYTECODE_BASE_GAS, 32_000);
        assert_eq!(BYTECODE_PER_BYTE_GAS, 200);
    }

    #[test]
    fn config_change_op_gas() {
        assert_eq!(CONFIG_CHANGE_OP_GAS, 20_000);
    }

    #[test]
    fn sload_gas() {
        assert_eq!(SLOAD_GAS, 2_100);
    }

    #[test]
    fn eoa_auth_gas() {
        assert_eq!(EOA_AUTH_GAS, 6_000);
    }

    #[test]
    fn custom_verifier_gas_cap() {
        assert_eq!(CUSTOM_VERIFIER_GAS_CAP, 200_000);
    }

    // ── Limits ────────────────────────────────────────────────────────

    #[test]
    fn max_calls_per_tx() {
        assert_eq!(MAX_CALLS_PER_TX, 100);
    }

    #[test]
    fn max_account_changes_per_tx() {
        assert_eq!(MAX_ACCOUNT_CHANGES_PER_TX, 10);
    }

    #[test]
    fn max_config_ops_per_tx() {
        assert_eq!(MAX_CONFIG_OPS_PER_TX, 5);
    }

    #[test]
    fn max_signature_size() {
        assert_eq!(MAX_SIGNATURE_SIZE, 2048);
    }

    #[test]
    fn nonce_key_max() {
        assert_eq!(NONCE_KEY_MAX, alloy_primitives::U256::MAX);
    }

    // ── Verifier gas costs baseline ───────────────────────────────────

    #[test]
    fn verifier_gas_costs_base_v1() {
        let costs = VerifierGasCosts::BASE_V1;
        assert_eq!(costs.k1, 6_000);
        assert_eq!(costs.p256_raw, 9_500);
        assert_eq!(costs.p256_webauthn, 15_000);
        assert_eq!(costs.delegate, 3_000);
    }

    // ── Owner scope bits ──────────────────────────────────────────────

    #[test]
    fn owner_scope_bits() {
        assert_eq!(OwnerScope::UNRESTRICTED, 0x00);
        assert_eq!(OwnerScope::SIGNATURE, 0x01);
        assert_eq!(OwnerScope::SENDER, 0x02);
        assert_eq!(OwnerScope::PAYER, 0x04);
        assert_eq!(OwnerScope::CONFIG, 0x08);
    }

    #[test]
    fn owner_scope_has_logic() {
        // UNRESTRICTED passes any check.
        assert!(OwnerScope::has(OwnerScope::UNRESTRICTED, OwnerScope::SENDER));
        assert!(OwnerScope::has(OwnerScope::UNRESTRICTED, OwnerScope::PAYER));

        // Specific bits.
        assert!(OwnerScope::has(OwnerScope::SENDER, OwnerScope::SENDER));
        assert!(!OwnerScope::has(OwnerScope::SENDER, OwnerScope::PAYER));

        // Combined bits.
        let combined = OwnerScope::SENDER | OwnerScope::PAYER;
        assert!(OwnerScope::has(combined, OwnerScope::SENDER));
        assert!(OwnerScope::has(combined, OwnerScope::PAYER));
        assert!(!OwnerScope::has(combined, OwnerScope::CONFIG));
    }

    // ── Storage slot computation consistency ──────────────────────────

    #[test]
    fn nonce_slot_is_deterministic() {
        let sender = Address::repeat_byte(0xAA);
        let key = alloy_primitives::U256::from(1);

        let slot1 = nonce_slot(sender, key);
        let slot2 = nonce_slot(sender, key);
        assert_eq!(slot1, slot2);

        // Different sender → different slot.
        let other = Address::repeat_byte(0xBB);
        let slot3 = nonce_slot(other, key);
        assert_ne!(slot1, slot3);
    }

    #[test]
    fn account_state_slot_is_deterministic() {
        let sender = Address::repeat_byte(0xAA);

        let slot1 = account_state_slot(sender);
        let slot2 = account_state_slot(sender);
        assert_eq!(slot1, slot2);
    }

    #[test]
    fn owner_config_slot_is_deterministic() {
        let sender = Address::repeat_byte(0xAA);
        let owner_id = alloy_primitives::B256::repeat_byte(0x11);

        let slot1 = owner_config_slot(sender, owner_id);
        let slot2 = owner_config_slot(sender, owner_id);
        assert_eq!(slot1, slot2);
    }
}
