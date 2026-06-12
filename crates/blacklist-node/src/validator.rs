//! FR-1 — chain-level blacklist ingress filter.
//!
//! Implements the upstream [`IngressBlacklistFilter`] (op-reth txpool) so the standard
//! `OpTransactionValidator` rejects, at mempool admission (RPC + P2P), any tx whose top-level
//! sender or recipient is blacklisted — without wrapping the validator type (which would
//! change `N::Pool` and break the OpAddOns stack, see [[upstream-component-type-pinning]]).
//! The filter is attached via `OpTransactionValidator::with_ingress_blacklist` by
//! [`crate::XLayerBlacklistPoolBuilder`]. This is a best-effort gate: a stale snapshot is not
//! a safety gap because the execution gate (FR-2/3) is the backstop (FR-1 AC2).

use crate::runtime::BlacklistRuntimeCtx;
use alloy_primitives::Address;
use reth_optimism_txpool::IngressBlacklistFilter;
use xlayer_blacklist::POOL_REJECT_MESSAGE;

/// Chain-level blacklist ingress filter backed by the shared [`BlacklistRuntimeCtx`] snapshot.
#[derive(Debug, Clone)]
pub struct XLayerIngressFilter {
    ctx: BlacklistRuntimeCtx,
}

impl XLayerIngressFilter {
    /// New filter sharing the chain-keyed runtime context (snapshot handle + metrics).
    pub fn new(ctx: BlacklistRuntimeCtx) -> Self {
        Self { ctx }
    }
}

impl IngressBlacklistFilter for XLayerIngressFilter {
    fn reject_reason(&self, from: Address, to: Option<Address>) -> Option<&'static str> {
        let snapshot = self.ctx.load_snapshot();
        // Disabled chain / empty list → pass everything through (FR-6 AC2).
        if snapshot.is_empty() {
            return None;
        }
        // op-geth order: check top-level `to` first (no signature recovery needed), then
        // `from`. Either hit → reject with the fixed, dynamic-field-free message (FR-7).
        if let Some(to) = to
            && snapshot.contains(&to)
        {
            self.ctx.record_pool_reject();
            return Some(POOL_REJECT_MESSAGE);
        }
        if snapshot.contains(&from) {
            self.ctx.record_pool_reject();
            return Some(POOL_REJECT_MESSAGE);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use xlayer_blacklist::BlacklistSnapshot;

    const AAA: Address = address!("00000000000000000000000000000000000000aa");
    const BBB: Address = address!("00000000000000000000000000000000000000bb");

    fn filter_with(addrs: &[Address]) -> XLayerIngressFilter {
        let ctx = BlacklistRuntimeCtx::new(195);
        let snap: BlacklistSnapshot = addrs.iter().copied().collect();
        ctx.store_snapshot(snap);
        XLayerIngressFilter::new(ctx)
    }

    #[test]
    fn empty_snapshot_passes() {
        let ctx = BlacklistRuntimeCtx::new(195); // enabled but empty list
        let f = XLayerIngressFilter::new(ctx);
        assert_eq!(f.reject_reason(AAA, Some(BBB)), None);
    }

    #[test]
    fn to_hit_rejects_with_fixed_message() {
        let f = filter_with(&[AAA]);
        assert_eq!(f.reject_reason(BBB, Some(AAA)), Some(POOL_REJECT_MESSAGE));
    }

    #[test]
    fn from_hit_rejects() {
        let f = filter_with(&[AAA]);
        assert_eq!(f.reject_reason(AAA, Some(BBB)), Some(POOL_REJECT_MESSAGE));
    }

    #[test]
    fn no_hit_passes() {
        let f = filter_with(&[AAA]);
        let ccc = address!("00000000000000000000000000000000000000cc");
        assert_eq!(f.reject_reason(BBB, Some(ccc)), None);
    }

    #[test]
    fn create_tx_checks_from_only() {
        let f = filter_with(&[AAA]);
        // contract creation (to = None): only `from` is checked.
        assert_eq!(f.reject_reason(AAA, None), Some(POOL_REJECT_MESSAGE));
        assert_eq!(f.reject_reason(BBB, None), None);
    }

    #[test]
    fn reject_message_is_fixed_and_field_free() {
        assert_eq!(
            POOL_REJECT_MESSAGE,
            "xlayer-blacklist: sender or recipient is on the blacklist"
        );
        assert!(!POOL_REJECT_MESSAGE.contains("0x"));
    }
}
