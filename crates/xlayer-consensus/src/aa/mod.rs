//! EIP-8130 account-abstraction transaction — wire-level types.

mod constants;
mod tx;
mod types;

pub use constants::{
    AA_BASE_COST, AA_PAYER_TYPE, AA_TX_TYPE_ID, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS,
    CONFIG_CHANGE_OP_GAS, CONFIG_CHANGE_SKIP_GAS, CUSTOM_VERIFIER_GAS_CAP, DEPLOYMENT_HEADER_SIZE,
    EOA_AUTH_GAS, MAX_ACCOUNT_CHANGES_PER_TX, MAX_CALLS_PER_TX, MAX_CONFIG_OPS_PER_TX,
    MAX_SIGNATURE_SIZE, NONCE_KEY_COLD_GAS, NONCE_KEY_MAX, NONCE_KEY_WARM_GAS, SLOAD_GAS,
};
pub use tx::TxEip8130;
pub use types::{
    AccountChangeEntry, Call, ConfigChangeEntry, CreateEntry, DelegationEntry, Owner, OwnerChange,
    OwnerScope, CHANGE_TYPE_CONFIG, CHANGE_TYPE_CREATE, CHANGE_TYPE_DELEGATION, OP_AUTHORIZE_OWNER,
    OP_REVOKE_OWNER,
};
