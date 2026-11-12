use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::client::ActionItem;

fn address(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn quota_entry(hash_byte: u8, origin: Address, nonce: u64, to: &str, token: &str) -> SubmitEntry {
    let hash = B256::repeat_byte(hash_byte);
    SubmitEntry {
        tx: SubmitTx {
            tx_hash: format!("{hash:#x}"),
            origin: format!("{origin:#x}"),
            contract_address: format!("{:#x}", address(0xee)),
            nonce,
            actions: BTreeMap::from([(
                "quota".to_string(),
                vec![ActionItem {
                    name: "transfer".to_string(),
                    address: token.to_string(),
                    params: BTreeMap::from([
                        ("from".to_string(), json!(format!("{origin:#x}"))),
                        ("to".to_string(), json!(to)),
                        ("value".to_string(), json!("1")),
                    ]),
                }],
            )]),
        },
        hash,
        generation: u64::from(hash_byte),
        origin,
    }
}

fn group(height: u64, entry: SubmitEntry) -> SubmitGroup {
    SubmitGroup::new(height, vec![entry])
}

fn normal_nodes(schedule: &[ExecutionSegment]) -> &[SubmitNode] {
    assert_eq!(schedule.len(), 1);
    let ExecutionSegment::Normal(nodes) = &schedule[0] else {
        panic!("expected one normal segment")
    };
    nodes
}

#[test]
fn empty_planner_has_no_segments() {
    assert!(build_submit_schedule(Vec::new()).is_empty());
}

#[test]
fn independent_groups_have_no_predecessors() {
    let groups = (1..=4)
        .map(|i| {
            group(
                100 + u64::from(i),
                quota_entry(
                    i,
                    address(i),
                    u64::from(i),
                    &format!("{:#x}", address(i + 10)),
                    &format!("{:#x}", address(i + 20)),
                ),
            )
        })
        .collect();
    let schedule = build_submit_schedule(groups);
    let nodes = normal_nodes(&schedule);
    assert!(nodes.iter().all(|node| node.remaining_predecessors == 0));
    assert!(nodes.iter().all(|node| node.successors.is_empty()));
}

#[test]
fn same_nonce_groups_form_nearest_predecessor_chain() {
    let origin = address(1);
    let groups = (1..=3)
        .map(|i| {
            group(
                100 + u64::from(i),
                quota_entry(
                    i,
                    origin,
                    7,
                    &format!("{:#x}", address(i + 10)),
                    &format!("{:#x}", address(i + 20)),
                ),
            )
        })
        .collect();
    let schedule = build_submit_schedule(groups);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes.iter().map(|node| node.remaining_predecessors).collect::<Vec<_>>(), [0, 1, 1]);
    assert_eq!(nodes[0].successors, [1]);
    assert_eq!(nodes[1].successors, [2]);
    assert!(nodes[2].successors.is_empty());
}

#[test]
fn same_quota_groups_form_nearest_predecessor_chain() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let groups = (1..=3)
        .map(|i| group(100 + u64::from(i), quota_entry(i, address(i), u64::from(i), &to, &token)))
        .collect();
    let schedule = build_submit_schedule(groups);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes.iter().map(|node| node.remaining_predecessors).collect::<Vec<_>>(), [0, 1, 1]);
    assert_eq!(nodes[0].successors, [1]);
    assert_eq!(nodes[1].successors, [2]);
}

#[test]
fn different_erc1155_ids_with_same_recipient_and_token_still_conflict() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut first = quota_entry(1, address(1), 1, &to, &token);
    let mut second = quota_entry(2, address(2), 2, &to, &token);
    for (entry, id) in [(&mut first, "7"), (&mut second, "8")] {
        entry.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
            ("operator".to_string(), json!(format!("{:#x}", address(20)))),
            ("from".to_string(), json!(format!("{:#x}", address(21)))),
            ("to".to_string(), json!(to.clone())),
            ("id".to_string(), json!(id)),
            ("value".to_string(), json!("1")),
        ]);
    }
    let schedule = build_submit_schedule(vec![group(100, first), group(101, second)]);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes[1].remaining_predecessors, 1);
    assert_eq!(nodes[0].successors, [1]);
}

#[test]
fn valid_erc1155_batch_shape_remains_non_exclusive() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut entry = quota_entry(1, address(1), 1, &to, &token);
    entry.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
        ("operator".to_string(), json!(format!("{:#x}", address(20)))),
        ("from".to_string(), json!(format!("{:#x}", address(21)))),
        ("to".to_string(), json!(to)),
        ("ids".to_string(), json!(["7", "8"])),
        ("values".to_string(), json!(["100", "200"])),
    ]);

    let schedule = build_submit_schedule(vec![group(100, entry)]);
    assert!(matches!(schedule.as_slice(), [ExecutionSegment::Normal(_)]));
}

#[test]
fn shared_nonce_and_quota_predecessor_is_counted_once() {
    let origin = address(1);
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let schedule = build_submit_schedule(vec![
        group(100, quota_entry(1, origin, 7, &to, &token)),
        group(101, quota_entry(2, origin, 7, &to, &token)),
    ]);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes[1].remaining_predecessors, 1);
    assert_eq!(nodes[0].successors, [1]);
}

#[test]
fn group_with_two_distinct_conflicts_waits_for_both_predecessors() {
    let origin_a = address(1);
    let origin_b = address(2);
    let to_x = format!("{:#x}", address(10));
    let to_y = format!("{:#x}", address(12));
    let token = format!("{:#x}", address(11));
    let schedule = build_submit_schedule(vec![
        group(100, quota_entry(1, origin_a, 7, &to_x, &token)),
        group(101, quota_entry(2, origin_b, 8, &to_y, &token)),
        group(102, quota_entry(3, origin_a, 7, &to_y, &token)),
    ]);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes[2].remaining_predecessors, 2);
    assert_eq!(nodes[0].successors, [2]);
    assert_eq!(nodes[1].successors, [2]);
}

#[test]
fn unknown_audit_type_is_an_exclusive_barrier() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut custom = quota_entry(2, address(2), 2, &to, &token);
    let items = custom.tx.actions.remove("quota").unwrap();
    custom.tx.actions.insert("custom".to_string(), items);
    let schedule = build_submit_schedule(vec![
        group(100, quota_entry(1, address(1), 1, &to, &token)),
        group(101, custom),
        group(102, quota_entry(3, address(3), 3, &to, &token)),
    ]);
    assert!(matches!(
        schedule.as_slice(),
        [ExecutionSegment::Normal(_), ExecutionSegment::Exclusive(_), ExecutionSegment::Normal(_)]
    ));
}

#[test]
fn malformed_or_empty_actions_are_exclusive() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut malformed = quota_entry(1, address(1), 1, &to, &token);
    malformed.tx.actions.get_mut("quota").unwrap()[0].params.remove("to");
    let mut empty = quota_entry(2, address(2), 2, &to, &token);
    empty.tx.actions.clear();
    let schedule = build_submit_schedule(vec![group(100, malformed), group(101, empty)]);
    assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
}

#[test]
fn empty_quota_items_and_invalid_token_are_exclusive() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut empty_items = quota_entry(1, address(1), 1, &to, &token);
    empty_items.tx.actions.get_mut("quota").unwrap().clear();
    let mut invalid_token = quota_entry(2, address(2), 2, &to, &token);
    invalid_token.tx.actions.get_mut("quota").unwrap()[0].address = "not-an-address".into();

    let schedule = build_submit_schedule(vec![group(100, empty_items), group(101, invalid_token)]);
    assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
}

#[test]
fn malformed_known_quota_shapes_are_exclusive() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let mut missing_value = quota_entry(1, address(1), 1, &to, &token);
    missing_value.tx.actions.get_mut("quota").unwrap()[0].params.remove("value");

    let mut invalid_value = quota_entry(2, address(2), 2, &to, &token);
    invalid_value.tx.actions.get_mut("quota").unwrap()[0]
        .params
        .insert("value".to_string(), json!({ "amount": "1" }));

    let mut mismatched_batch = quota_entry(3, address(3), 3, &to, &token);
    mismatched_batch.tx.actions.get_mut("quota").unwrap()[0].params = BTreeMap::from([
        ("operator".to_string(), json!(format!("{:#x}", address(20)))),
        ("from".to_string(), json!(format!("{:#x}", address(21)))),
        ("to".to_string(), json!(to.clone())),
        ("ids".to_string(), json!(["1", "2"])),
        ("values".to_string(), json!(["3"])),
    ]);

    let mut unknown_field = quota_entry(4, address(4), 4, &to, &token);
    unknown_field.tx.actions.get_mut("quota").unwrap()[0]
        .params
        .insert("resource".to_string(), json!("unexpected"));

    let schedule = build_submit_schedule(vec![
        group(100, missing_value),
        group(101, invalid_value),
        group(102, mismatched_batch),
        group(103, unknown_field),
    ]);
    assert!(schedule.iter().all(|segment| matches!(segment, ExecutionSegment::Exclusive(_))));
}

#[test]
fn address_keys_are_compared_after_typed_normalization() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let upper_to = format!("0x{}", to[2..].to_uppercase());
    let upper_token = format!("0x{}", token[2..].to_uppercase());
    let schedule = build_submit_schedule(vec![
        group(100, quota_entry(1, address(1), 1, &to, &token)),
        group(101, quota_entry(2, address(2), 2, &upper_to, &upper_token)),
    ]);
    let nodes = normal_nodes(&schedule);
    assert_eq!(nodes[1].remaining_predecessors, 1);
    assert_eq!(nodes[0].successors, [1]);
}

#[test]
fn dependency_edges_always_point_from_lower_to_higher_height() {
    let to = format!("{:#x}", address(10));
    let token = format!("{:#x}", address(11));
    let origin = address(1);
    let schedule = build_submit_schedule(vec![
        group(100, quota_entry(1, origin, 7, &to, &token)),
        group(101, quota_entry(2, origin, 7, &to, &token)),
        group(102, quota_entry(3, origin, 7, &to, &token)),
    ]);
    let nodes = normal_nodes(&schedule);
    for (predecessor, node) in nodes.iter().enumerate() {
        let predecessor_height = node.group.as_ref().unwrap().block_height;
        for successor in &node.successors {
            let successor_height = nodes[*successor].group.as_ref().unwrap().block_height;
            assert!(predecessor < *successor);
            assert!(predecessor_height < successor_height);
        }
    }
}

#[test]
fn batch_stats_track_actual_in_flight_and_release_slots() {
    let stats = SubmitBatchStats::default();
    assert_eq!(stats.max_in_flight(), 0);

    let first = stats.enter();
    let second = stats.enter();
    assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 2);
    assert_eq!(stats.max_in_flight(), 2);

    drop(first);
    assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 1);
    drop(second);
    assert_eq!(stats.current_in_flight.load(Ordering::Relaxed), 0);
    assert_eq!(stats.max_in_flight(), 2);
}
