//! Storage slot derivation for EIP-8130 system contracts.

use alloy_primitives::{keccak256, Address, B256, U256};

/// Base storage slot for `_ownerConfig` mapping.
pub const OWNER_CONFIG_BASE_SLOT: U256 = U256::ZERO;

/// Base storage slot for `_accountState` mapping.
pub const ACCOUNT_STATE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Alias for callers watching the lock portion.
pub const LOCK_BASE_SLOT: U256 = ACCOUNT_STATE_BASE_SLOT;

/// Alias for callers watching the sequence portion.
pub const SEQUENCE_BASE_SLOT: U256 = ACCOUNT_STATE_BASE_SLOT;

/// Base storage slot for `nonce` mapping in NonceManager.
pub const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Base storage slot for `expiringNonceSeen` mapping.
pub const EXPIRING_SEEN_BASE_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);

/// Base storage slot for `expiringNonceRing` mapping.
pub const EXPIRING_RING_BASE_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);

/// Direct storage slot holding the ring-buffer pointer.
pub const EXPIRING_RING_PTR_SLOT: U256 = U256::from_limbs([4, 0, 0, 0]);

/// Packed `AccountState` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountState {
    /// Sequence for multi-chain config changes.
    pub multichain_sequence: u64,
    /// Sequence for local-chain config changes.
    pub local_sequence: u64,
    /// Timestamp after which the account becomes unlocked.
    pub unlocks_at: u64,
    /// Delay configured when entering the locked state.
    pub unlock_delay: u16,
}

/// Computes the storage slot for `owner_config[ownerId][account]`.
pub fn owner_config_slot(account: Address, owner_id: B256) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(owner_id.as_slice());
        OWNER_CONFIG_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
            |(i, &b)| {
                buf[32 + i] = b;
            },
        );
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(account.as_slice());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Computes the storage slot for `nonce[account][nonceKey]`.
pub fn nonce_slot(account: Address, nonce_key: U256) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        NONCE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(|(i, &b)| {
            buf[32 + i] = b;
        });
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(&nonce_key.to_be_bytes::<32>());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Computes the storage slot for `expiringNonceSeen[txHash]`.
pub fn expiring_seen_slot(tx_hash: B256) -> B256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(tx_hash.as_slice());
    EXPIRING_SEEN_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
        |(i, &b)| {
            buf[32 + i] = b;
        },
    );
    keccak256(buf)
}

/// Computes the storage slot for `expiringNonceRing[index]`.
pub fn expiring_ring_slot(index: u32) -> B256 {
    let mut buf = [0u8; 64];
    buf[28..32].copy_from_slice(&index.to_be_bytes());
    EXPIRING_RING_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
        |(i, &b)| {
            buf[32 + i] = b;
        },
    );
    keccak256(buf)
}

/// Computes the storage slot for `_accountState[account]`.
pub fn account_state_slot(account: Address) -> B256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(account.as_slice());
    ACCOUNT_STATE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
        |(i, &b)| {
            buf[32 + i] = b;
        },
    );
    keccak256(buf)
}

/// Computes the lock storage slot.
pub fn lock_slot(account: Address) -> B256 {
    account_state_slot(account)
}

/// Computes the sequence storage slot.
pub fn sequence_base_slot(account: Address) -> B256 {
    account_state_slot(account)
}

/// Parses a packed `_accountState[account]` word.
pub fn parse_account_state(slot_value: U256) -> AccountState {
    let bytes = slot_value.to_be_bytes::<32>();
    let multichain_sequence = u64::from_be_bytes(bytes[24..32].try_into().expect("8-byte slice"));
    let local_sequence = u64::from_be_bytes(bytes[16..24].try_into().expect("8-byte slice"));
    let mut unlocks_at_bytes = [0u8; 8];
    unlocks_at_bytes[3..8].copy_from_slice(&bytes[11..16]);
    let unlocks_at = u64::from_be_bytes(unlocks_at_bytes);
    let unlock_delay = u16::from_be_bytes([bytes[9], bytes[10]]);
    AccountState { multichain_sequence, local_sequence, unlocks_at, unlock_delay }
}

/// Encodes an [`AccountState`] into the packed Solidity storage layout.
pub fn encode_account_state(state: AccountState) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&state.multichain_sequence.to_be_bytes());
    bytes[16..24].copy_from_slice(&state.local_sequence.to_be_bytes());
    let unlocks_at_bytes = state.unlocks_at.to_be_bytes();
    bytes[11..16].copy_from_slice(&unlocks_at_bytes[3..8]);
    bytes[9..11].copy_from_slice(&state.unlock_delay.to_be_bytes());
    U256::from_be_bytes(bytes)
}

/// Reads a sequence from a packed slot value.
pub fn read_sequence(slot_value: U256, is_multichain: bool) -> u64 {
    let state = parse_account_state(slot_value);
    if is_multichain {
        state.multichain_sequence
    } else {
        state.local_sequence
    }
}

/// Writes a sequence value into the packed word.
pub fn write_sequence(current: U256, is_multichain: bool, new_value: u64) -> U256 {
    let mut state = parse_account_state(current);
    if is_multichain {
        state.multichain_sequence = new_value;
    } else {
        state.local_sequence = new_value;
    }
    encode_account_state(state)
}

/// Parses an `owner_config` storage slot value into `(verifier, scope)`.
pub fn parse_owner_config(slot_value: B256) -> (Address, u8) {
    let bytes = slot_value.as_slice();
    let verifier = Address::from_slice(&bytes[12..32]);
    let scope = bytes[11];
    (verifier, scope)
}

/// Encodes `(verifier, scope)` into an `owner_config` storage slot value.
pub fn encode_owner_config(verifier: Address, scope: u8) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(verifier.as_slice());
    bytes[11] = scope;
    B256::from(bytes)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256};

    use super::*;

    #[test]
    fn owner_config_deterministic() {
        let account = Address::repeat_byte(0x01);
        let owner_id = B256::repeat_byte(0x02);
        assert_eq!(owner_config_slot(account, owner_id), owner_config_slot(account, owner_id));
    }

    #[test]
    fn owner_config_slot_matches_solidity_fixture() {
        let account = address!("0x1234567890abcdef1234567890abcdef12345678");
        let owner_id = b256!("0x0f0e0d0c0b0a09080706050403020100fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0");
        let expected = b256!("0x0164bdd9c38961717a4b6760c1c55c6f0ba5648be63c9b386ce9c654795c0017");
        assert_eq!(owner_config_slot(account, owner_id), expected);
    }

    #[test]
    fn nonce_slot_deterministic() {
        let account = Address::repeat_byte(0x01);
        let nonce_key = U256::from(42);
        assert_eq!(nonce_slot(account, nonce_key), nonce_slot(account, nonce_key));
    }

    #[test]
    fn account_state_slot_matches_solidity_fixture() {
        let account = address!("0x1234567890abcdef1234567890abcdef12345678");
        let expected = b256!("0x99e61528d26b8142c5975683c5533b7afbe872a9ef426c9e9cfe43f5c9ce53a4");
        assert_eq!(account_state_slot(account), expected);
        assert_eq!(lock_slot(account), expected);
        assert_eq!(sequence_base_slot(account), expected);
    }

    #[test]
    fn parse_encode_account_state_roundtrip() {
        let state = AccountState {
            multichain_sequence: 7,
            local_sequence: 9,
            unlocks_at: 0x0102_0304_05,
            unlock_delay: 0x0607,
        };
        let encoded = encode_account_state(state);
        assert_eq!(parse_account_state(encoded), state);
    }

    #[test]
    fn write_sequence_preserves_lock_fields() {
        let packed = encode_account_state(AccountState {
            multichain_sequence: 1,
            local_sequence: 2,
            unlocks_at: 0x0102_0304_05,
            unlock_delay: 0x0607,
        });
        let updated = write_sequence(packed, true, 99);
        let state = parse_account_state(updated);
        assert_eq!(state.multichain_sequence, 99);
        assert_eq!(state.local_sequence, 2);
        assert_eq!(state.unlocks_at, 0x0102_0304_05);
        assert_eq!(state.unlock_delay, 0x0607);
    }

    #[test]
    fn parse_encode_owner_config_roundtrip() {
        let verifier = Address::repeat_byte(0xAA);
        let scope = 0x0F;
        let encoded = encode_owner_config(verifier, scope);
        let (decoded_verifier, decoded_scope) = parse_owner_config(encoded);
        assert_eq!(decoded_verifier, verifier);
        assert_eq!(decoded_scope, scope);
    }

    #[test]
    fn parse_zero_slot_returns_zero() {
        let (verifier, scope) = parse_owner_config(B256::ZERO);
        assert_eq!(verifier, Address::ZERO);
        assert_eq!(scope, 0);
    }

    #[test]
    fn expiring_seen_slot_deterministic() {
        let hash = B256::repeat_byte(0xAA);
        assert_eq!(expiring_seen_slot(hash), expiring_seen_slot(hash));
    }

    #[test]
    fn expiring_ring_slot_deterministic() {
        assert_eq!(expiring_ring_slot(42), expiring_ring_slot(42));
        assert_ne!(expiring_ring_slot(0), expiring_ring_slot(1));
    }
}
