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

use alloy_consensus::BlockHeader;
use alloy_primitives::{Address, U256};
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{error::INTERNAL_ERROR_CODE, ErrorObjectOwned},
};
use op_revm::{
    constants::{
        DELEGATE_VERIFIER_ADDRESS, K1_VERIFIER_ADDRESS, P256_RAW_VERIFIER_ADDRESS,
        P256_WEBAUTHN_VERIFIER_ADDRESS,
    },
    gas_params::{xlayer_gas_params, XlayerGasParams},
    OpSpecId,
};
use reth_chainspec::ChainSpecProvider;
use reth_optimism_forks::OpHardforks;
use reth_storage_api::BlockReaderIdExt;
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

/// Server implementation: derives the active fork on each RPC call from
/// the latest block's timestamp + chainspec, so the response tracks fork
/// activation rather than baking in a single [`OpSpecId`]. Pre-XLAYER_V1
/// forks return zero `maxAuthCost` values for the four AA verifier slots
/// (gas slots aren't populated yet) but still advertise the addresses so
/// wallets can pre-build for the upcoming fork.
#[derive(Debug, Clone)]
pub struct AcceptedVerifiersImpl<Provider> {
    provider: Provider,
}

impl<Provider> AcceptedVerifiersImpl<Provider> {
    /// Wraps a provider for chainspec + latest-header lookups.
    pub const fn new(provider: Provider) -> Self {
        Self { provider }
    }
}

/// Builds the verifier-policy response for a given fork.
pub fn build_response_for_spec(spec: OpSpecId) -> AcceptedVerifiersResponse {
    if spec < OpSpecId::XLAYER_V1 {
        return AcceptedVerifiersResponse {
            accepts_unknown_verifiers: false,
            verifiers: Vec::new(),
        };
    }

    let params = xlayer_gas_params(spec);

    // Worst-case Delegate budget = outer + most expensive native inner.
    // We pick P256 WebAuthn (the most expensive native verifier) so the
    // advertised cap covers any legal inner verifier. The handler enforces
    // the per-call cost separately; this is purely a client hint about
    // what the node will allow.
    let delegate_max_auth_cost = params
        .delegate_outer_verification_gas()
        .saturating_add(params.p256_webauthn_verification_gas());

    AcceptedVerifiersResponse {
        // Custom verifiers run via STATICCALL with the
        // XLAYER_AA_CUSTOM_VERIFIER_GAS_CAP budget today, so they're
        // effectively accepted. A strict-allowlist policy would flip this
        // to `false` once node-level enforcement lands.
        accepts_unknown_verifiers: true,
        verifiers: vec![
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
        ],
    }
}

#[async_trait::async_trait]
impl<Provider> AcceptedVerifiersServer for AcceptedVerifiersImpl<Provider>
where
    Provider: BlockReaderIdExt + ChainSpecProvider + Send + Sync + 'static,
    <Provider as ChainSpecProvider>::ChainSpec: OpHardforks,
{
    async fn get_accepted_verifiers(&self) -> RpcResult<AcceptedVerifiersResponse> {
        // Pick the active spec from the latest sealed header's timestamp.
        // Falling back to timestamp 0 (pre-Bedrock) for a missing header
        // produces an `OpSpecId::BEDROCK` response — the one safe answer
        // when we can't establish the chain's current fork state.
        let timestamp = self
            .provider
            .latest_header()
            .map_err(|e| rpc_internal_error(format!("latest header lookup failed: {e}")))?
            .map(|h| h.timestamp())
            .unwrap_or(0);
        let spec =
            alloy_op_evm::spec_by_timestamp_after_bedrock(&*self.provider.chain_spec(), timestamp);
        Ok(build_response_for_spec(spec))
    }
}

fn rpc_internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(INTERNAL_ERROR_CODE, msg, None::<()>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the four native addresses + the XLAYER_V1 gas figures so any
    /// silent change to either upstream side surfaces here rather than
    /// in a hard-to-trace field-level RPC mismatch.
    #[test]
    fn xlayer_v1_response_matches_constants_and_gas_table() {
        let r = build_response_for_spec(OpSpecId::XLAYER_V1);
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
        let r = build_response_for_spec(OpSpecId::XLAYER_V1);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"acceptsUnknownVerifiers\":true"));
        assert!(json.contains("\"maxAuthCost\""));
        assert!(!json.contains("accepts_unknown_verifiers"));
        assert!(!json.contains("max_auth_cost"));
    }

    /// Pre-XLAYER_V1 forks return an empty verifier list with
    /// `acceptsUnknownVerifiers: false` — the AA infrastructure isn't
    /// active yet, and clients must not interpret the response as
    /// "AA accepted today."
    #[test]
    fn pre_xlayer_v1_returns_empty_response_with_acceptance_disabled() {
        let r = build_response_for_spec(OpSpecId::BEDROCK);
        assert!(!r.accepts_unknown_verifiers, "AA not active pre-XLAYER_V1");
        assert!(r.verifiers.is_empty(), "no verifiers advertised before fork activation");
    }

    /// Boundary case: every fork from BEDROCK up to (but not including)
    /// XLAYER_V1 must produce the empty response. Catches regressions
    /// where a new intermediate fork is introduced and the gating cutoff
    /// silently shifts.
    #[test]
    fn forks_strictly_before_xlayer_v1_are_all_gated() {
        for spec in [
            OpSpecId::BEDROCK,
            OpSpecId::REGOLITH,
            OpSpecId::CANYON,
            OpSpecId::ECOTONE,
            OpSpecId::FJORD,
            OpSpecId::GRANITE,
            OpSpecId::HOLOCENE,
            OpSpecId::ISTHMUS,
            OpSpecId::JOVIAN,
        ] {
            let r = build_response_for_spec(spec);
            assert!(!r.accepts_unknown_verifiers, "{spec:?}: must not accept AA pre-XLAYER_V1");
            assert!(r.verifiers.is_empty(), "{spec:?}: must not advertise verifiers pre-XLAYER_V1");
        }
    }
}
