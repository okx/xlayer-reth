//! L2-block snapshot pinning: a rule hot reload becomes visible only at the next block's payload
//! job, never between two transactions or two flashblocks of the block being built.

use super::*;
use alloy_primitives::Address;
use rcs_filter::rules::{load_rules, RawRule};
use rcs_filter::{FilterConfig, FilterHandle, SystemClock};
use serde_json::json;

fn selector_rule(id: &str, address: Address) -> RawRule {
    serde_json::from_value(json!({
        "id": id,
        "event_abis": {},
        "callstack": { "direct_delegatecalls": { "entry": format!("{address:#x}") } },
        "condition": { "!=": [ { "var": "entry.delegateTo" }, null ] },
        "action": "deny"
    }))
    .expect("valid rule")
}

fn handle_with(address: Address, content_version: u64) -> Arc<FilterHandle> {
    let rules = load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        content_version,
        vec![selector_rule("guard", address)],
    );
    assert!(rules.rejected.is_empty(), "{:?}", rules.rejected);
    Arc::new(FilterHandle::for_test(FilterConfig::default(), rules, Arc::new(SystemClock)))
}

/// A context carrying the snapshot pinned for its block, exactly as the payload job builds it.
fn pin(filter: &Arc<FilterHandle>) -> Option<Arc<rcs_filter::rules::RuleSet>> {
    Some(filter.rule_snapshot())
}

#[test]
fn the_capture_config_comes_from_the_pinned_snapshot_not_the_live_one() {
    let first = Address::repeat_byte(0x11);
    let second = Address::repeat_byte(0x22);
    let filter = handle_with(first, 1);

    // The block's payload job pins the snapshot once, up front.
    let pinned = pin(&filter).expect("screening is enabled");
    let pinned_stamp = pinned.stamp();
    assert_eq!(pinned.callstack_selectors(), [first]);

    // A hot reload lands mid-block. Rule installation is an immediate swap, so this is the
    // moment that would leak new rules into the block already being built.
    filter.install_rules(load_rules(
        rcs_filter::config::MIN_PROTOCOL_VERSION,
        2,
        vec![selector_rule("guard", second)],
    ));

    // Every flashblock of this block builds a fresh inspector from the SAME pinned snapshot,
    // so neither the selector set nor the stamp moves part-way through the block.
    assert_eq!(pinned.callstack_selectors(), [first]);
    assert_eq!(pinned.stamp(), pinned_stamp);

    // The next block's payload job pins the reloaded snapshot, which is where it takes effect.
    let next_block = pin(&filter).expect("screening is enabled");
    assert_eq!(next_block.callstack_selectors(), [second]);
    assert_ne!(next_block.stamp(), pinned_stamp);
}

#[test]
fn a_context_without_screening_pins_no_snapshot_and_captures_nothing() {
    // With the master switch off there is no snapshot to pin, and the capture configuration
    // is inert, so the observation hooks cost nothing.
    let config = CallstackCaptureConfig::default();
    assert!(config.is_inert());
}

#[test]
fn a_snapshot_with_no_selectors_yields_an_inert_capture_config() {
    // Screening enabled but no active rule asking about call frames: the whole observation
    // path stays inert rather than observing every frame "just in case".
    let rules = load_rules(rcs_filter::config::MIN_PROTOCOL_VERSION, 1, vec![]);
    assert!(!rules.has_callstack_rules());
    let config = CallstackCaptureConfig::new(rules.stamp(), rules.callstack_selectors().to_vec());
    assert!(config.is_inert());
}

#[test]
fn a_snapshot_with_selectors_yields_an_active_capture_config_carrying_its_stamp() {
    let address = Address::repeat_byte(0x33);
    let filter = handle_with(address, 7);
    let pinned = filter.rule_snapshot();
    let config = CallstackCaptureConfig::new(pinned.stamp(), pinned.callstack_selectors().to_vec());
    assert!(!config.is_inert());
    // The stamp travels with the selector set, so an inspector cannot be configured from one
    // snapshot and stamped with another.
    assert_eq!(config.stamp_for_test(), Some(pinned.stamp()));
}
