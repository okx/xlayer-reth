# Autopilot Execution Report: per-ip-ws-rate-limit

## Execution Summary

| Phase | Status | Details |
|-------|--------|---------|
| Phase 0 | PASSED | IMPL generated from PRD |
| Phase 1 | PASSED | Baseline cargo check passed |
| Phase 2 | PASSED | 4/4 step gates, clippy clean |
| Phase 3 | PASSED | 1 round, 1 issue fixed (DashMap deadlock) |
| Phase 4 | PASSED | Knowledge consolidated |
| Phase 5 | PASSED | Committed |

## Files Modified

| File | Change |
|------|--------|
| `crates/builder/src/args/op.rs` | Added `ws_per_ip_limit` and `ws_idle_timeout_secs` CLI args |
| `crates/builder/src/flashblocks/mod.rs` | Added config fields + plumbing |
| `crates/builder/src/broadcast/wspub.rs` | Main implementation: per-IP counters, idle sweep, subscriber guard, tests |
| `crates/builder/src/metrics/builder.rs` | Added 5 new metrics fields |
| `crates/builder/src/default/service.rs` | Passed new params to WebSocketPublisher::new |
| `crates/builder/src/flashblocks/service.rs` | Passed new params to WebSocketPublisher::new |
| `crates/builder/src/broadcast/mod.rs` | Updated test helper with new params |
| `context-kg/technical/pitfalls/concurrency-issues.md` | Added DashMap deadlock pitfall |

## Tests

- `test_subscriber_guard_decrements_on_drop` — PASS
- `test_per_ip_counter_increment_decrement` — PASS
- `test_per_ip_limit_rejects_excess_connections` — PASS
- `test_sweeper_identifies_idle_connections` — PASS
- All 23 existing broadcast tests — PASS (no regression)

## Decisions Made

1. Removed unused `ip` field from `ConnHandle` (clippy dead_code warning). IP tracking is in `SubscriberGuard` instead.
2. Fixed DashMap deadlock pattern: `.get()` Ref released before calling `.remove_if()` on same map.

## Skipped Suggestions

None.

## Review Issues Fixed

1. **DashMap Ref deadlock** (Critical): `SubscriberGuard::drop` and global-limit rollback both held a read lock via `.get()` while calling `.remove_if()` which needs a write lock on the same shard. Fixed by extracting the value from the Ref via `.map()` before calling mutating methods.

## Next Steps

- Push branch and create MR (handled by Stage 3)
- Enable on testnet with default limits
- Monitor `ws_subscribers_per_ip_max` metric for NAT/proxy issues
