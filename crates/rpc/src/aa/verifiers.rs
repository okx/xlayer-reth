//! `eth_getAcceptedVerifiers` — EIP-8130 RPC method that exposes the
//! node's verifier acceptance policy.
//!
//! Per the spec, the response advertises which verifier contract
//! addresses the node will admit transactions for, whether the node has
//! a native (precompile-style) implementation of each, and the gas
//! budget the node imposes on the verifier's auth execution. Wallets
//! and dapps call this method to discover which signature algorithm /
//! verifier contract they can target on a particular RPC node before
//! constructing a 0x7B transaction.
//!
//! ## What we report today
//!
//! The four native verifiers wired into the handler at
//! [`alloy_op_evm::eip8130::native_verifier::NativeVerifier`] —
//! K1 / P256 raw / P256 WebAuthn / Delegate — each with `native: true`
//! and `maxAuthCost` pulled from the active fork's
//! [`xlayer_gas_params`].
//!
//! `acceptsUnknownVerifiers` is hardcoded to `true` because the
//! handler runs `STATICCALL` against any non-native verifier address
//! within [`op_revm::constants::XLAYER_AA_CUSTOM_VERIFIER_GAS_CAP`].
//! When a declarative acceptance-policy registry lands (per-node CLI /
//! chainspec config), this flag becomes a real toggle.

use alloy_primitives::{Address, U256};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use op_revm::{
    constants::{
        DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
        P256_WEBAUTHN_VERIFIER_ADDRESS,
    },
    gas_params::{xlayer_gas_params, XlayerGasParams},
    OpSpecId,
};
use serde::{Deserialize, Serialize};

/// JSON-RPC response for `eth_getAcceptedVerifiers`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedVerifiersResponse {
    /// Whether the node admits transactions naming verifiers that are
    /// not in the [`Self::verifiers`] allowlist. The handler currently
    /// runs custom verifiers via `STATICCALL` within a global gas cap,
    /// so this is `true`; flipping to a strict allowlist would require
    /// a node-side enforcement step that does not yet exist.
    pub accepts_unknown_verifiers: bool,
    /// Verifiers the node explicitly knows about. Each entry tags
    /// whether the node has a native fast-path implementation and the
    /// gas budget for the verifier's auth execution.
    pub verifiers: Vec<AcceptedVerifier>,
}

/// One entry in [`AcceptedVerifiersResponse::verifiers`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedVerifier {
    /// Verifier contract address (the on-chain identifier the
    /// transaction's `sender_auth` blob prefixes itself with).
    pub address: Address,
    /// `true` iff the node has a built-in implementation that
    /// short-circuits the EVM `STATICCALL` path. False would indicate a
    /// verifier the node accepts but executes via the EVM.
    pub native: bool,
    /// Gas budget the node will allow for this verifier's auth
    /// execution, expressed as a `U256` for spec / Base parity. For
    /// native verifiers this is the fork-bound verification gas; for
    /// the Delegate verifier it is the worst-case outer + max native
    /// inner cost (P256 WebAuthn) since the inner verifier is chosen
    /// by the user.
    pub max_auth_cost: U256,
}

/// Server trait for `eth_getAcceptedVerifiers`.
///
/// Registered after the upstream `eth` module so reth's last-wins
/// method routing makes this the active handler for the camelCased
/// `eth_getAcceptedVerifiers` method (no upstream conflict since reth
/// does not implement this method itself).
#[rpc(server, namespace = "eth")]
pub trait AcceptedVerifiers {
    /// Returns the node's verifier acceptance policy.
    #[method(name = "getAcceptedVerifiers")]
    async fn get_accepted_verifiers(&self) -> RpcResult<AcceptedVerifiersResponse>;
}

/// Server implementation: snapshots the response at construction time
/// from a fixed [`OpSpecId`] (the fork whose gas table the node should
/// advertise — typically [`OpSpecId::XLAYER_V1`], where the AA verifier
/// gas slots first acquire non-zero values).
#[derive(Debug, Clone)]
pub struct AcceptedVerifiersImpl {
    response: AcceptedVerifiersResponse,
}

impl AcceptedVerifiersImpl {
    /// Convenience constructor: snapshot the response at the
    /// `XLAYER_V1` fork — the only one where AA verifier gas slots are
    /// populated. Use this when wiring the RPC into the node binary
    /// without pulling [`OpSpecId`] in to the call site.
    pub fn for_xlayer_v1() -> Self {
        Self::new(OpSpecId::XLAYER_V1)
    }

    /// Builds the response for a given fork. Pre-`XLAYER_V1` forks
    /// produce zero `maxAuthCost` values for the four AA verifier
    /// slots — they're not active yet — but the addresses are
    /// reported regardless so wallets can pre-build for the upcoming
    /// fork without round-tripping fork activation status.
    pub fn new(spec: OpSpecId) -> Self {
        let params = xlayer_gas_params(spec);

        // Worst-case Delegate budget = outer + most expensive native inner.
        // We pick P256 WebAuthn (the most expensive native verifier) so
        // the advertised cap covers any legal inner verifier. The handler
        // enforces the per-call cost separately; this is purely a client
        // hint about what the node will allow.
        let delegate_max_auth_cost = params
            .delegate_outer_verification_gas()
            .saturating_add(params.p256_webauthn_verification_gas());

        let verifiers = vec![
            AcceptedVerifier {
                address: K1_VERIFIER_ADDRESS,
                native: true,
                max_auth_cost: U256::from(params.k1_verification_gas()),
            },
            AcceptedVerifier {
                address: P256_RAW_VERIFIER_ADDRESS,
                native: true,
                max_auth_cost: U256::from(params.p256_raw_verification_gas()),
            },
            AcceptedVerifier {
                address: P256_WEBAUTHN_VERIFIER_ADDRESS,
                native: true,
                max_auth_cost: U256::from(params.p256_webauthn_verification_gas()),
            },
            AcceptedVerifier {
                address: DELEGATE_VERIFIER_ADDRESS,
                native: true,
                max_auth_cost: U256::from(delegate_max_auth_cost),
            },
        ];

        Self {
            response: AcceptedVerifiersResponse {
                // Custom verifiers run via STATICCALL with the
                // XLAYER_AA_CUSTOM_VERIFIER_GAS_CAP budget today, so
                // they're effectively accepted. A strict-allowlist
                // policy would flip this to `false` once node-level
                // enforcement lands.
                accepts_unknown_verifiers: true,
                verifiers,
            },
        }
    }
}

#[async_trait::async_trait]
impl AcceptedVerifiersServer for AcceptedVerifiersImpl {
    async fn get_accepted_verifiers(&self) -> RpcResult<AcceptedVerifiersResponse> {
        Ok(self.response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the four native addresses + the XLAYER_V1 gas figures so any
    /// silent change to either upstream side surfaces here rather than
    /// in a hard-to-trace field-level RPC mismatch.
    #[test]
    fn xlayer_v1_response_matches_constants_and_gas_table() {
        let r = AcceptedVerifiersImpl::new(OpSpecId::XLAYER_V1).response;
        assert!(r.accepts_unknown_verifiers);
        assert_eq!(r.verifiers.len(), 4);

        let by_addr = |addr: Address| {
            r.verifiers.iter().find(|v| v.address == addr).expect("verifier present")
        };

        let k1 = by_addr(K1_VERIFIER_ADDRESS);
        assert!(k1.native);
        assert_eq!(k1.max_auth_cost, U256::from(6_000u64));

        let p256_raw = by_addr(P256_RAW_VERIFIER_ADDRESS);
        assert!(p256_raw.native);
        assert_eq!(p256_raw.max_auth_cost, U256::from(9_500u64));

        let webauthn = by_addr(P256_WEBAUTHN_VERIFIER_ADDRESS);
        assert!(webauthn.native);
        assert_eq!(webauthn.max_auth_cost, U256::from(15_000u64));

        let delegate = by_addr(DELEGATE_VERIFIER_ADDRESS);
        assert!(delegate.native);
        // outer (6000) + worst-case inner (P256 WebAuthn: 15000) = 21000
        assert_eq!(delegate.max_auth_cost, U256::from(21_000u64));
    }

    /// JSON shape parity with Base / spec: camelCase `acceptsUnknownVerifiers`,
    /// `maxAuthCost`. Without this pin, a stray serde rename on either
    /// of the response structs goes unnoticed until a real client breaks.
    #[test]
    fn response_serializes_with_camel_case_keys() {
        let r = AcceptedVerifiersImpl::new(OpSpecId::XLAYER_V1).response;
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"acceptsUnknownVerifiers\":true"));
        assert!(json.contains("\"maxAuthCost\""));
        assert!(!json.contains("accepts_unknown_verifiers"));
        assert!(!json.contains("max_auth_cost"));
    }

    #[tokio::test]
    async fn handler_returns_snapshot_unchanged() {
        let api = AcceptedVerifiersImpl::new(OpSpecId::XLAYER_V1);
        let response = AcceptedVerifiersServer::get_accepted_verifiers(&api).await.unwrap();
        assert_eq!(response, api.response);
    }
}
