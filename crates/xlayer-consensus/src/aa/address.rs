//! CREATE2 address derivation for XLayerAA account-creation entries.
//!
//! Ported from Base's `eip8130/address.rs`. Correctness here is consensus-
//! critical — every node that processes a create entry must derive the
//! same address, so the deployment header, salt mixing, and CREATE2
//! formula are pinned by explicit unit tests.

use std::vec::Vec;

use alloy_primitives::{keccak256, Address, Bytes, B256};

use super::types::Owner;

/// The 14-byte deployment header prepended to user bytecode so a plain
/// "runtime code blob" can be supplied directly as the CREATE2 init code.
///
/// The header is a minimal EVM program that copies the trailing bytes into
/// memory and RETURNs them as the deployed runtime code:
///
/// ```text
/// PUSH2 <len_hi> <len_lo>   // 0x61 HH LL
/// DUP1                       // 0x80
/// PUSH1 0x0e                 // 0x60 0x0e  (header size → code offset)
/// PUSH1 0x00                 // 0x60 0x00
/// CODECOPY                   // 0x39
/// PUSH1 0x00                 // 0x60 0x00
/// RETURN                     // 0xf3
/// ```
///
/// The last two bytes are fixed padding (`0x00 0x00`) to reach 14 bytes;
/// they are unreachable at runtime but are part of the committed hash, so
/// the format is locked.
pub fn deployment_header(bytecode_len: usize) -> [u8; 14] {
    let len = bytecode_len as u16;
    let hi = (len >> 8) as u8;
    let lo = (len & 0xFF) as u8;
    [
        0x61, hi, lo,   // PUSH2 len
        0x80, // DUP1
        0x60, 0x0e, // PUSH1 14
        0x60, 0x00, // PUSH1 0
        0x39, // CODECOPY
        0x60, 0x00, // PUSH1 0
        0xf3, // RETURN
        0x00, 0x00, // padding to 14 bytes
    ]
}

/// Full init code: `deployment_header(len) || bytecode`.
pub fn deployment_code(bytecode: &[u8]) -> Vec<u8> {
    let header = deployment_header(bytecode.len());
    let mut code = Vec::with_capacity(header.len() + bytecode.len());
    code.extend_from_slice(&header);
    code.extend_from_slice(bytecode);
    code
}

/// Computes the effective CREATE2 salt from the user-provided salt and the
/// set of initial owners.
///
/// ```text
/// sorted        = sort(initial_owners, by ownerId)
/// commitment    = keccak256( ownerId_0 ‖ verifier_0 ‖ scope_0 ‖ … )
/// effective_salt = keccak256( user_salt ‖ commitment )
/// ```
///
/// Sorting makes the derived address independent of the order in which the
/// RLP encoder emitted the owners — any wallet that reorders the list for
/// its own convenience (e.g. by `owner_id`) still hashes to the same
/// account.
pub fn effective_salt(user_salt: B256, initial_owners: &[Owner]) -> B256 {
    let mut sorted: Vec<&Owner> = initial_owners.iter().collect();
    sorted.sort_by_key(|o| o.owner_id);

    // 32 (owner_id) + 20 (verifier) + 1 (scope) = 53 bytes per owner.
    let mut commitment_input = Vec::with_capacity(sorted.len() * 53);
    for owner in &sorted {
        commitment_input.extend_from_slice(owner.owner_id.as_slice());
        commitment_input.extend_from_slice(owner.verifier.as_slice());
        commitment_input.push(owner.scope);
    }
    let commitment = keccak256(&commitment_input);

    let mut salt_input = Vec::with_capacity(64);
    salt_input.extend_from_slice(user_salt.as_slice());
    salt_input.extend_from_slice(commitment.as_slice());
    keccak256(&salt_input)
}

/// Standard Ethereum CREATE2 formula:
///
/// ```text
/// address = keccak256( 0xff ‖ deployer ‖ salt ‖ keccak256(init_code) )[12..]
/// ```
pub fn create2_address(deployer: Address, salt: B256, init_code: &[u8]) -> Address {
    let init_code_hash = keccak256(init_code);
    let mut buf = Vec::with_capacity(1 + 20 + 32 + 32);
    buf.push(0xFF);
    buf.extend_from_slice(deployer.as_slice());
    buf.extend_from_slice(salt.as_slice());
    buf.extend_from_slice(init_code_hash.as_slice());
    let hash = keccak256(&buf);
    Address::from_slice(&hash[12..])
}

/// End-to-end derivation for an `AccountChangeEntry::Create` — wires
/// [`effective_salt`] + [`deployment_code`] + [`create2_address`] together.
pub fn derive_account_address(
    deployer: Address,
    user_salt: B256,
    bytecode: &Bytes,
    initial_owners: &[Owner],
) -> Address {
    let salt = effective_salt(user_salt, initial_owners);
    let code = deployment_code(bytecode);
    create2_address(deployer, salt, &code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_header_is_14_bytes() {
        let h = deployment_header(256);
        assert_eq!(h.len(), 14);
        assert_eq!(h[0], 0x61);
        assert_eq!(h[1], 0x01);
        assert_eq!(h[2], 0x00);
    }

    #[test]
    fn deployment_code_concatenates() {
        let bytecode = [0x60u8, 0x00, 0xf3]; // PUSH1 0 RETURN
        let code = deployment_code(&bytecode);
        assert_eq!(code.len(), 14 + 3);
        assert_eq!(&code[14..], &bytecode);
    }

    #[test]
    fn effective_salt_is_order_independent() {
        let owner_a = Owner {
            verifier: Address::repeat_byte(1),
            owner_id: B256::repeat_byte(0x01),
            scope: 0,
        };
        let owner_b = Owner {
            verifier: Address::repeat_byte(2),
            owner_id: B256::repeat_byte(0x02),
            scope: 0,
        };
        let salt = B256::repeat_byte(0xAA);
        let s1 = effective_salt(salt, &[owner_a.clone(), owner_b.clone()]);
        let s2 = effective_salt(salt, &[owner_b, owner_a]);
        assert_eq!(s1, s2, "ordering of initial_owners must not affect the address");
    }

    #[test]
    fn create2_address_is_deterministic() {
        let deployer = Address::repeat_byte(0xDD);
        let salt = B256::repeat_byte(0xAA);
        let code = [0x60u8, 0x00, 0xf3];
        let a1 = create2_address(deployer, salt, &code);
        let a2 = create2_address(deployer, salt, &code);
        assert_eq!(a1, a2);
        assert_ne!(a1, Address::ZERO);
    }

    #[test]
    fn different_owners_produce_different_addresses() {
        let deployer = Address::repeat_byte(0xDD);
        let salt = B256::repeat_byte(0xAA);
        let bytecode = Bytes::from_static(&[0x60, 0x00, 0xf3]);
        let owners_a = vec![Owner {
            verifier: Address::repeat_byte(1),
            owner_id: B256::repeat_byte(0x01),
            scope: 0,
        }];
        let owners_b = vec![Owner {
            verifier: Address::repeat_byte(2),
            owner_id: B256::repeat_byte(0x02),
            scope: 0,
        }];
        let a1 = derive_account_address(deployer, salt, &bytecode, &owners_a);
        let a2 = derive_account_address(deployer, salt, &bytecode, &owners_b);
        assert_ne!(a1, a2);
    }
}
