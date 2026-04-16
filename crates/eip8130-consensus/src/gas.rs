//! EIP-8130 intrinsic gas calculation.

use alloy_primitives::Address;
use alloy_rlp::Encodable;

use super::{
    constants::{
        VerifierGasCosts, AA_BASE_COST, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
        CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, EXPIRING_NONCE_GAS, NONCE_KEY_COLD_GAS,
        NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, SLOAD_GAS,
    },
    predeploys::{DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS},
    AccountChangeEntry, TxEip8130,
};

/// Extracts the inner verifier address for a DELEGATE auth blob.
pub fn delegate_inner_verifier(auth: &[u8]) -> Option<Address> {
    if auth.len() >= 60 && Address::from_slice(&auth[..20]) == DELEGATE_VERIFIER_ADDRESS {
        let inner = Address::from_slice(&auth[40..60]);
        if inner == Address::ZERO {
            Some(K1_VERIFIER_ADDRESS)
        } else {
            Some(inner)
        }
    } else {
        None
    }
}

/// Computes the intrinsic gas for an AA transaction.
pub fn intrinsic_gas(tx: &TxEip8130, nonce_key_is_warm: bool, chain_id: u64) -> u64 {
    intrinsic_gas_with_costs(tx, nonce_key_is_warm, chain_id, &VerifierGasCosts::BASE_V1)
}

/// Computes intrinsic gas with explicit verifier gas costs.
pub fn intrinsic_gas_with_costs(
    tx: &TxEip8130,
    nonce_key_is_warm: bool,
    chain_id: u64,
    costs: &VerifierGasCosts,
) -> u64 {
    let sender_inner = delegate_inner_verifier(&tx.sender_auth);
    let payer_inner = delegate_inner_verifier(&tx.payer_auth);

    let mut gas = AA_BASE_COST;

    gas += tx_payload_cost(tx);
    gas += sender_auth_cost(tx);
    gas += payer_auth_cost(tx);
    gas += total_verification_gas(tx, costs, sender_inner, payer_inner);
    gas += authorizer_verification_gas(tx, costs);
    if tx.nonce_key != NONCE_KEY_MAX {
        gas += nonce_key_cost(nonce_key_is_warm);
    } else {
        gas += EXPIRING_NONCE_GAS;
    }
    gas += bytecode_cost(tx);
    gas += account_changes_cost(tx, chain_id);

    gas
}

/// Standard EIP-2028 calldata cost over the full RLP encoding.
pub fn tx_payload_cost(tx: &TxEip8130) -> u64 {
    let mut buf = Vec::with_capacity(tx.length());
    tx.encode(&mut buf);
    calldata_gas(&buf)
}

/// Sender authentication overhead (SLOAD for owner_config read).
pub fn sender_auth_cost(_tx: &TxEip8130) -> u64 {
    SLOAD_GAS
}

/// Payer authentication cost: 0 for self-pay.
pub fn payer_auth_cost(tx: &TxEip8130) -> u64 {
    if tx.is_self_pay() {
        0
    } else {
        SLOAD_GAS
    }
}

/// Gas for native sender verification.
pub fn sender_verification_gas(
    tx: &TxEip8130,
    costs: &VerifierGasCosts,
    inner_verifier: Option<Address>,
) -> u64 {
    if tx.is_eoa() || tx.sender_auth.len() < 20 {
        costs.gas_for_verifier(K1_VERIFIER_ADDRESS, None)
    } else {
        let verifier = Address::from_slice(&tx.sender_auth[..20]);
        costs.gas_for_verifier(verifier, inner_verifier)
    }
}

/// Gas for native payer verification.
pub fn payer_verification_gas(
    tx: &TxEip8130,
    costs: &VerifierGasCosts,
    inner_verifier: Option<Address>,
) -> u64 {
    if tx.is_self_pay() {
        return 0;
    }
    if tx.payer_auth.len() < 20 {
        return costs.gas_for_verifier(K1_VERIFIER_ADDRESS, None);
    }
    let verifier = Address::from_slice(&tx.payer_auth[..20]);
    costs.gas_for_verifier(verifier, inner_verifier)
}

/// Total verification gas for both sender and payer.
pub fn total_verification_gas(
    tx: &TxEip8130,
    costs: &VerifierGasCosts,
    sender_inner: Option<Address>,
    payer_inner: Option<Address>,
) -> u64 {
    sender_verification_gas(tx, costs, sender_inner)
        + payer_verification_gas(tx, costs, payer_inner)
}

/// Gas for config change authorizer verification.
pub fn authorizer_verification_gas(tx: &TxEip8130, costs: &VerifierGasCosts) -> u64 {
    let mut gas = 0u64;
    for entry in &tx.account_changes {
        if let AccountChangeEntry::ConfigChange(cc) = entry {
            if cc.authorizer_auth.len() < 20 {
                continue;
            }
            let verifier = Address::from_slice(&cc.authorizer_auth[..20]);
            let inner = delegate_inner_verifier(&cc.authorizer_auth);
            gas += costs.gas_for_verifier(verifier, inner);
            gas += SLOAD_GAS;
        }
    }
    gas
}

/// Nonce key cost: cold vs warm.
pub fn nonce_key_cost(is_warm: bool) -> u64 {
    if is_warm {
        NONCE_KEY_WARM_GAS
    } else {
        NONCE_KEY_COLD_GAS
    }
}

/// Bytecode deployment cost.
pub fn bytecode_cost(tx: &TxEip8130) -> u64 {
    for entry in &tx.account_changes {
        if let AccountChangeEntry::Create(create) = entry {
            if create.bytecode.is_empty() {
                return BYTECODE_BASE_GAS;
            }
            return BYTECODE_BASE_GAS + BYTECODE_PER_BYTE_GAS * create.bytecode.len() as u64;
        }
    }
    0
}

/// Configuration change cost.
pub fn account_changes_cost(tx: &TxEip8130, chain_id: u64) -> u64 {
    let mut gas = 0u64;
    for entry in &tx.account_changes {
        match entry {
            AccountChangeEntry::Create(create) => {
                gas += CONFIG_CHANGE_OP_GAS * (1 + create.initial_owners.len() as u64);
            }
            AccountChangeEntry::ConfigChange(cc) => {
                if cc.chain_id == 0 || cc.chain_id == chain_id {
                    gas += CONFIG_CHANGE_OP_GAS * cc.owner_changes.len() as u64;
                } else {
                    gas += CONFIG_CHANGE_SKIP_GAS;
                }
            }
            AccountChangeEntry::Delegation(_) => {
                gas += super::constants::BYTECODE_PER_BYTE_GAS * 23;
            }
        }
    }
    gas
}

/// Counts account-change units in a transaction.
pub fn account_change_units(tx: &TxEip8130) -> usize {
    tx.account_changes
        .iter()
        .map(|entry| match entry {
            AccountChangeEntry::Create(create) => 1 + create.initial_owners.len(),
            AccountChangeEntry::ConfigChange(cc) => cc.owner_changes.len(),
            AccountChangeEntry::Delegation(_) => 1,
        })
        .sum()
}

/// EIP-2028 calldata gas: 16 per non-zero byte, 4 per zero byte.
fn calldata_gas(data: &[u8]) -> u64 {
    data.iter().fold(0u64, |acc, &byte| acc + if byte == 0 { 4 } else { 16 })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes, B256, U256};

    use super::*;
    use crate::{
        predeploys::{
            DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
            P256_WEBAUTHN_VERIFIER_ADDRESS,
        },
        types::{ConfigChangeEntry, CreateEntry, Owner, OwnerChange},
    };

    #[test]
    fn calldata_gas_basic() {
        assert_eq!(calldata_gas(&[0, 0, 1, 2]), 4 + 4 + 16 + 16);
        assert_eq!(calldata_gas(&[]), 0);
    }

    #[test]
    fn nonce_key_cost_warm_vs_cold() {
        assert_eq!(nonce_key_cost(false), 22_100);
        assert_eq!(nonce_key_cost(true), 5_000);
    }

    #[test]
    fn bytecode_cost_no_create() {
        let tx = TxEip8130::default();
        assert_eq!(bytecode_cost(&tx), 0);
    }

    #[test]
    fn bytecode_cost_with_create() {
        let tx = TxEip8130 {
            account_changes: vec![AccountChangeEntry::Create(CreateEntry {
                user_salt: Default::default(),
                bytecode: Bytes::from_static(&[0x60; 100]),
                initial_owners: vec![Owner {
                    verifier: Address::repeat_byte(1),
                    owner_id: Default::default(),
                    scope: 0,
                }],
            })],
            ..Default::default()
        };
        assert_eq!(bytecode_cost(&tx), 32_000 + 200 * 100);
    }

    #[test]
    fn account_changes_cost_applied_vs_skipped() {
        let tx = TxEip8130 {
            account_changes: vec![
                AccountChangeEntry::ConfigChange(ConfigChangeEntry {
                    chain_id: 8453,
                    sequence: 0,
                    owner_changes: vec![
                        OwnerChange {
                            change_type: 0x01,
                            verifier: Address::repeat_byte(1),
                            owner_id: Default::default(),
                            scope: 0,
                        },
                        OwnerChange {
                            change_type: 0x01,
                            verifier: Address::repeat_byte(2),
                            owner_id: Default::default(),
                            scope: 0,
                        },
                    ],
                    authorizer_auth: Bytes::new(),
                }),
                AccountChangeEntry::ConfigChange(ConfigChangeEntry {
                    chain_id: 1,
                    sequence: 0,
                    owner_changes: vec![OwnerChange {
                        change_type: 0x01,
                        verifier: Address::repeat_byte(3),
                        owner_id: Default::default(),
                        scope: 0,
                    }],
                    authorizer_auth: Bytes::new(),
                }),
            ],
            ..Default::default()
        };
        let cost = account_changes_cost(&tx, 8453);
        assert_eq!(cost, 2 * CONFIG_CHANGE_OP_GAS + CONFIG_CHANGE_SKIP_GAS);
    }

    #[test]
    fn intrinsic_gas_smoke() {
        let mut auth = K1_VERIFIER_ADDRESS.as_slice().to_vec();
        auth.extend_from_slice(&[0xAB; 65]);
        let tx = TxEip8130 {
            chain_id: 8453,
            from: Some(Address::repeat_byte(1)),
            nonce_key: U256::ZERO,
            nonce_sequence: 0,
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        let gas = intrinsic_gas(&tx, true, 8453);
        assert!(gas >= AA_BASE_COST);
    }

    #[test]
    fn sender_verification_gas_for_each_verifier_type() {
        let costs = VerifierGasCosts::BASE_V1;

        // EOA uses K1
        let tx = TxEip8130 {
            from: None,
            sender_auth: Bytes::from_static(&[0xAB; 65]),
            ..Default::default()
        };
        assert_eq!(sender_verification_gas(&tx, &costs, None), 6_000);

        // Configured K1
        let mut auth = K1_VERIFIER_ADDRESS.as_slice().to_vec();
        auth.push(0xAB);
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(1)),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        assert_eq!(sender_verification_gas(&tx, &costs, None), 6_000);

        // Configured P256 raw
        let mut auth = P256_RAW_VERIFIER_ADDRESS.as_slice().to_vec();
        auth.push(0xAB);
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(1)),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        assert_eq!(sender_verification_gas(&tx, &costs, None), 9_500);

        // Configured P256 WebAuthn
        let mut auth = P256_WEBAUTHN_VERIFIER_ADDRESS.as_slice().to_vec();
        auth.push(0xAB);
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(1)),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        assert_eq!(sender_verification_gas(&tx, &costs, None), 15_000);

        // Custom verifier returns 0
        let custom_verifier = Address::repeat_byte(0xCC);
        let mut auth = custom_verifier.as_slice().to_vec();
        auth.extend_from_slice(&[0xDD; 20]);
        let tx = TxEip8130 {
            from: Some(Address::repeat_byte(1)),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        assert_eq!(sender_verification_gas(&tx, &costs, None), 0);
    }

    #[test]
    fn delegate_inner_verifier_extraction() {
        let mut delegate_k1 = DELEGATE_VERIFIER_ADDRESS.as_slice().to_vec();
        delegate_k1.extend_from_slice(Address::repeat_byte(0x11).as_slice());
        delegate_k1.extend_from_slice(K1_VERIFIER_ADDRESS.as_slice());
        delegate_k1.push(0xAB);
        assert_eq!(delegate_inner_verifier(&delegate_k1), Some(K1_VERIFIER_ADDRESS));

        let mut delegate_p256 = DELEGATE_VERIFIER_ADDRESS.as_slice().to_vec();
        delegate_p256.extend_from_slice(Address::repeat_byte(0x22).as_slice());
        delegate_p256.extend_from_slice(P256_RAW_VERIFIER_ADDRESS.as_slice());
        assert_eq!(delegate_inner_verifier(&delegate_p256), Some(P256_RAW_VERIFIER_ADDRESS));

        // Non-delegate
        let mut k1_only = K1_VERIFIER_ADDRESS.as_slice().to_vec();
        k1_only.push(0xAB);
        assert_eq!(delegate_inner_verifier(&k1_only), None);

        // Too short
        assert_eq!(delegate_inner_verifier(&[]), None);
    }

    #[test]
    fn account_change_units_counts() {
        let tx = TxEip8130 {
            account_changes: vec![
                AccountChangeEntry::Create(CreateEntry {
                    user_salt: B256::repeat_byte(0xAA),
                    bytecode: Bytes::new(),
                    initial_owners: vec![
                        Owner {
                            verifier: Address::repeat_byte(1),
                            owner_id: B256::repeat_byte(0x10),
                            scope: 0,
                        },
                        Owner {
                            verifier: Address::repeat_byte(2),
                            owner_id: B256::repeat_byte(0x11),
                            scope: 0,
                        },
                    ],
                }),
                AccountChangeEntry::ConfigChange(ConfigChangeEntry {
                    chain_id: 8453,
                    sequence: 0,
                    owner_changes: vec![
                        OwnerChange {
                            change_type: 0x01,
                            verifier: Address::repeat_byte(3),
                            owner_id: B256::repeat_byte(0x12),
                            scope: 0,
                        },
                        OwnerChange {
                            change_type: 0x02,
                            verifier: Address::repeat_byte(4),
                            owner_id: B256::repeat_byte(0x13),
                            scope: 0,
                        },
                    ],
                    authorizer_auth: Bytes::new(),
                }),
            ],
            ..Default::default()
        };
        // create(1) + initial owners(2) + config ops(2) = 5
        assert_eq!(account_change_units(&tx), 5);
    }
}
