//! XLayerAA (EIP-8130) sub-types: calls, owners, account-change entries.

use std::vec::Vec;

use alloy_primitives::{Address, Bytes, B256};
use alloy_rlp::{length_of_length, BufMut, Decodable, Encodable, Header};

// ---------------------------------------------------------------------------
// Call
// ---------------------------------------------------------------------------

/// A single call inside a phase: RLP-encoded as `[to, data]`.
///
/// Value transfers are encoded separately — the handler derives the per-call
/// value from the phase layout, keeping the wire representation lean.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct Call {
    /// Target address.
    pub to: Address,
    /// Calldata.
    pub data: Bytes,
}

impl Encodable for Call {
    fn encode(&self, out: &mut dyn BufMut) {
        let payload = self.to.length() + self.data.length();
        Header { list: true, payload_length: payload }.encode(out);
        self.to.encode(out);
        self.data.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.to.length() + self.data.length();
        payload + length_of_length(payload)
    }
}

impl Decodable for Call {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        Ok(Self { to: Decodable::decode(buf)?, data: Decodable::decode(buf)? })
    }
}

// ---------------------------------------------------------------------------
// Owner
// ---------------------------------------------------------------------------

/// An initial owner registered at account creation: `[verifier, ownerId, scope]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct Owner {
    /// Verifier contract address.
    pub verifier: Address,
    /// Verifier-derived owner identifier.
    pub owner_id: B256,
    /// Permission bitmask; see [`OwnerScope`].
    pub scope: u8,
}

impl Encodable for Owner {
    fn encode(&self, out: &mut dyn BufMut) {
        let payload = self.verifier.length() + self.owner_id.length() + self.scope.length();
        Header { list: true, payload_length: payload }.encode(out);
        self.verifier.encode(out);
        self.owner_id.encode(out);
        self.scope.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.verifier.length() + self.owner_id.length() + self.scope.length();
        payload + length_of_length(payload)
    }
}

impl Decodable for Owner {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        Ok(Self {
            verifier: Decodable::decode(buf)?,
            owner_id: Decodable::decode(buf)?,
            scope: Decodable::decode(buf)?,
        })
    }
}

// ---------------------------------------------------------------------------
// OwnerScope
// ---------------------------------------------------------------------------

/// Permission bitmask namespace for an owner. A scope of `0x00`
/// ([`OwnerScope::UNRESTRICTED`]) is taken as "all permissions".
#[derive(Debug)]
pub struct OwnerScope;

impl OwnerScope {
    /// Owner may produce ERC-1271 signatures on behalf of the account.
    pub const SIGNATURE: u8 = 0x01;
    /// Owner may act as the transaction sender.
    pub const SENDER: u8 = 0x02;
    /// Owner may act as the transaction payer (gas sponsor).
    pub const PAYER: u8 = 0x04;
    /// Owner may authorize configuration changes.
    pub const CONFIG: u8 = 0x08;
    /// Unrestricted — any permission is granted.
    pub const UNRESTRICTED: u8 = 0x00;

    /// Returns `true` if `scope` grants `permission`. Unrestricted scopes
    /// (`0x00`) satisfy every check.
    pub const fn has(scope: u8, permission: u8) -> bool {
        scope == Self::UNRESTRICTED || (scope & permission) != 0
    }
}

// ---------------------------------------------------------------------------
// OwnerChange
// ---------------------------------------------------------------------------

/// Operation type byte: authorize an owner.
pub const OP_AUTHORIZE_OWNER: u8 = 0x01;
/// Operation type byte: revoke an owner.
pub const OP_REVOKE_OWNER: u8 = 0x02;

/// A single owner-change operation: `[change_type, verifier, ownerId, scope]`.
///
/// For revoke (`OP_REVOKE_OWNER`) the `verifier` and `scope` fields are still
/// encoded but the execution path ignores them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct OwnerChange {
    /// `0x01` = authorize, `0x02` = revoke.
    pub change_type: u8,
    /// Verifier contract address.
    pub verifier: Address,
    /// Owner identifier.
    pub owner_id: B256,
    /// Permission scope bitmask.
    pub scope: u8,
}

impl Encodable for OwnerChange {
    fn encode(&self, out: &mut dyn BufMut) {
        let payload = self.change_type.length()
            + self.verifier.length()
            + self.owner_id.length()
            + self.scope.length();
        Header { list: true, payload_length: payload }.encode(out);
        self.change_type.encode(out);
        self.verifier.encode(out);
        self.owner_id.encode(out);
        self.scope.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.change_type.length()
            + self.verifier.length()
            + self.owner_id.length()
            + self.scope.length();
        payload + length_of_length(payload)
    }
}

impl Decodable for OwnerChange {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        Ok(Self {
            change_type: Decodable::decode(buf)?,
            verifier: Decodable::decode(buf)?,
            owner_id: Decodable::decode(buf)?,
            scope: Decodable::decode(buf)?,
        })
    }
}

// ---------------------------------------------------------------------------
// AccountChangeEntry
// ---------------------------------------------------------------------------

/// Entry type byte: CREATE2 account deployment.
pub const CHANGE_TYPE_CREATE: u8 = 0x00;
/// Entry type byte: owner-configuration change batch.
pub const CHANGE_TYPE_CONFIG: u8 = 0x01;
/// Entry type byte: EIP-7702-style delegation set/clear.
pub const CHANGE_TYPE_DELEGATION: u8 = 0x02;

/// A single `account_changes` entry.
///
/// RLP layouts:
/// - `Create`:       `[0x00, user_salt, bytecode, [owner, …]]`
/// - `ConfigChange`: `[0x01, chain_id, sequence, [owner_change, …], authorizer_auth]`
/// - `Delegation`:   `[0x02, target]`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
pub enum AccountChangeEntry {
    /// Deploy a new account via CREATE2.
    Create(CreateEntry),
    /// Apply a batch of owner-configuration changes.
    ConfigChange(ConfigChangeEntry),
    /// Set or clear EIP-7702-style code delegation.
    Delegation(DelegationEntry),
}

/// Account-creation entry payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct CreateEntry {
    /// User-chosen salt for CREATE2 address derivation.
    pub user_salt: B256,
    /// Bytecode to deploy. Empty selects the default account proxy.
    pub bytecode: Bytes,
    /// Initial owners registered at creation. MUST be sorted by `owner_id`.
    pub initial_owners: Vec<Owner>,
}

/// Config-change entry payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ConfigChangeEntry {
    /// Target chain id; `0` means the entry applies to every chain
    /// (multi-chain sequence).
    pub chain_id: u64,
    /// Expected change sequence number on the target chain.
    pub sequence: u64,
    /// Owner changes to apply atomically.
    pub owner_changes: Vec<OwnerChange>,
    /// Authorization blob from an owner with `CONFIG` scope.
    pub authorizer_auth: Bytes,
}

/// Delegation entry payload: set EIP-7702 code delegation to `target`, or
/// clear an existing delegation by setting `target` to `Address::ZERO`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct DelegationEntry {
    /// Target implementation contract, or `Address::ZERO` to clear.
    pub target: Address,
}

impl Encodable for AccountChangeEntry {
    fn encode(&self, out: &mut dyn BufMut) {
        match self {
            Self::Create(c) => {
                let owners_payload: usize = c.initial_owners.iter().map(Encodable::length).sum();
                let payload = CHANGE_TYPE_CREATE.length()
                    + c.user_salt.length()
                    + c.bytecode.length()
                    + length_of_length(owners_payload)
                    + owners_payload;
                Header { list: true, payload_length: payload }.encode(out);
                CHANGE_TYPE_CREATE.encode(out);
                c.user_salt.encode(out);
                c.bytecode.encode(out);
                Header { list: true, payload_length: owners_payload }.encode(out);
                for owner in &c.initial_owners {
                    owner.encode(out);
                }
            }
            Self::ConfigChange(cc) => {
                let ops_payload: usize = cc.owner_changes.iter().map(Encodable::length).sum();
                let payload = CHANGE_TYPE_CONFIG.length()
                    + cc.chain_id.length()
                    + cc.sequence.length()
                    + length_of_length(ops_payload)
                    + ops_payload
                    + cc.authorizer_auth.length();
                Header { list: true, payload_length: payload }.encode(out);
                CHANGE_TYPE_CONFIG.encode(out);
                cc.chain_id.encode(out);
                cc.sequence.encode(out);
                Header { list: true, payload_length: ops_payload }.encode(out);
                for op in &cc.owner_changes {
                    op.encode(out);
                }
                cc.authorizer_auth.encode(out);
            }
            Self::Delegation(d) => {
                let payload = CHANGE_TYPE_DELEGATION.length() + d.target.length();
                Header { list: true, payload_length: payload }.encode(out);
                CHANGE_TYPE_DELEGATION.encode(out);
                d.target.encode(out);
            }
        }
    }

    fn length(&self) -> usize {
        match self {
            Self::Create(c) => {
                let owners_payload: usize = c.initial_owners.iter().map(Encodable::length).sum();
                let payload = CHANGE_TYPE_CREATE.length()
                    + c.user_salt.length()
                    + c.bytecode.length()
                    + length_of_length(owners_payload)
                    + owners_payload;
                payload + length_of_length(payload)
            }
            Self::ConfigChange(cc) => {
                let ops_payload: usize = cc.owner_changes.iter().map(Encodable::length).sum();
                let payload = CHANGE_TYPE_CONFIG.length()
                    + cc.chain_id.length()
                    + cc.sequence.length()
                    + length_of_length(ops_payload)
                    + ops_payload
                    + cc.authorizer_auth.length();
                payload + length_of_length(payload)
            }
            Self::Delegation(d) => {
                let payload = CHANGE_TYPE_DELEGATION.length() + d.target.length();
                payload + length_of_length(payload)
            }
        }
    }
}

impl Decodable for AccountChangeEntry {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let remaining = buf.len();

        let type_byte: u8 = Decodable::decode(buf)?;
        let this = match type_byte {
            CHANGE_TYPE_CREATE => {
                let user_salt = Decodable::decode(buf)?;
                let bytecode = Decodable::decode(buf)?;
                let owners_header = Header::decode(buf)?;
                if !owners_header.list {
                    return Err(alloy_rlp::Error::UnexpectedString);
                }
                let owners_end = buf.len() - owners_header.payload_length;
                let mut initial_owners = Vec::new();
                while buf.len() > owners_end {
                    initial_owners.push(Decodable::decode(buf)?);
                }
                Self::Create(CreateEntry { user_salt, bytecode, initial_owners })
            }
            CHANGE_TYPE_CONFIG => {
                let chain_id = Decodable::decode(buf)?;
                let sequence = Decodable::decode(buf)?;
                let ops_header = Header::decode(buf)?;
                if !ops_header.list {
                    return Err(alloy_rlp::Error::UnexpectedString);
                }
                let ops_end = buf.len() - ops_header.payload_length;
                let mut owner_changes = Vec::new();
                while buf.len() > ops_end {
                    owner_changes.push(Decodable::decode(buf)?);
                }
                let authorizer_auth = Decodable::decode(buf)?;
                Self::ConfigChange(ConfigChangeEntry {
                    chain_id,
                    sequence,
                    owner_changes,
                    authorizer_auth,
                })
            }
            CHANGE_TYPE_DELEGATION => {
                let target = Decodable::decode(buf)?;
                Self::Delegation(DelegationEntry { target })
            }
            _ => return Err(alloy_rlp::Error::Custom("invalid account change type byte")),
        };

        if buf.len() + header.payload_length != remaining {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        Ok(this)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_rlp_round_trip() {
        let call = Call { to: Address::repeat_byte(0xAB), data: Bytes::from_static(&[1, 2, 3, 4]) };
        let mut buf = Vec::new();
        call.encode(&mut buf);
        let decoded = Call::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(call, decoded);
    }

    #[test]
    fn owner_rlp_round_trip() {
        let owner = Owner {
            verifier: Address::repeat_byte(0x01),
            owner_id: B256::repeat_byte(0x02),
            scope: OwnerScope::SENDER | OwnerScope::CONFIG,
        };
        let mut buf = Vec::new();
        owner.encode(&mut buf);
        let decoded = Owner::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(owner, decoded);
    }

    #[test]
    fn create_entry_rlp_round_trip() {
        let entry = AccountChangeEntry::Create(CreateEntry {
            user_salt: B256::repeat_byte(0xAA),
            bytecode: Bytes::from_static(&[0x60, 0x00]),
            initial_owners: vec![Owner {
                verifier: Address::repeat_byte(0x01),
                owner_id: B256::repeat_byte(0x02),
                scope: 0,
            }],
        });
        let mut buf = Vec::new();
        entry.encode(&mut buf);
        let decoded = AccountChangeEntry::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn config_change_entry_rlp_round_trip() {
        let entry = AccountChangeEntry::ConfigChange(ConfigChangeEntry {
            chain_id: 196,
            sequence: 3,
            owner_changes: vec![OwnerChange {
                change_type: OP_AUTHORIZE_OWNER,
                verifier: Address::repeat_byte(0x01),
                owner_id: B256::repeat_byte(0x99),
                scope: OwnerScope::SENDER,
            }],
            authorizer_auth: Bytes::from_static(&[0xFF; 65]),
        });
        let mut buf = Vec::new();
        entry.encode(&mut buf);
        let decoded = AccountChangeEntry::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn delegation_entry_rlp_round_trip() {
        let entry =
            AccountChangeEntry::Delegation(DelegationEntry { target: Address::repeat_byte(0x42) });
        let mut buf = Vec::new();
        entry.encode(&mut buf);
        let decoded = AccountChangeEntry::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn owner_scope_has() {
        assert!(OwnerScope::has(OwnerScope::UNRESTRICTED, OwnerScope::SENDER));
        assert!(OwnerScope::has(OwnerScope::SENDER, OwnerScope::SENDER));
        assert!(!OwnerScope::has(OwnerScope::PAYER, OwnerScope::SENDER));
        assert!(OwnerScope::has(OwnerScope::SENDER | OwnerScope::PAYER, OwnerScope::PAYER));
    }

    #[test]
    fn unknown_account_change_type_rejected() {
        // Manual construction of `[0xFF, ...]` to simulate a rogue type byte.
        let mut buf = Vec::new();
        Header { list: true, payload_length: 1 }.encode(&mut buf);
        0xFFu8.encode(&mut buf);
        assert!(AccountChangeEntry::decode(&mut buf.as_slice()).is_err());
    }
}
