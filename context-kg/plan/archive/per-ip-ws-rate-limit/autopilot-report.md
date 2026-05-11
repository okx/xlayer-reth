# Autopilot Execution Report: Per-IP WebSocket Subscriber Rate Limit + Idle Sweep

## Execution Summary

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 0: PRD → IMPL | PASSED | Generated IMPL-per-ip-ws-rate-limit.md |
| Phase 1: Pre-flight | PASSED | Baseline compilation OK (11m 20s) |
| Phase 2: Implement | PASSED | 5 steps, all gates passed, clippy clean |
| Phase 3: Self-Review | PASSED | 0 critical, 0 medium issues |
| Phase 4: Checklist | SKIPPED | No existing checklist.md |
| Phase 4.5: Knowledge | SKIPPED | No reviews generated new items |
| Phase 5: Report + Commit | PASSED | |

## Steps Executed

### Step 1: CLI Arguments + Config (op.rs, flashblocks/mod.rs)
- Added `--flashblocks.ws-per-ip-limit` (default 4, env FLASHBLOCK_WS_PER_IP_LIMIT)
- Added `--flashblocks.ws-idle-timeout-secs` (default 90, env FLASHBLOCK_WS_IDLE_TIMEOUT_SECS)
- Added fields to FlashblocksConfig + Default + TryFrom

### Step 2: Per-IP Counter + ConnectionGuard (wspub.rs)
- `PerIpCounters = Arc<DashMap<IpAddr, Arc<AtomicUsize>>>`
- `ConnectionGuard` with Drop impl that decrements both counters
- Fixed DashMap deadlock: get() Ref must be dropped before remove_if() write lock

### Step 3: Idle-Timeout Sweep (wspub.rs)
- `idle_sweep_loop`: tokio interval 15s, checks last_active vs timeout
- `ConnHandle` with `last_active: Arc<AtomicU64>` + `close_tx: mpsc::Sender`
- `broadcast_loop` selects on close_rx for graceful shutdown

### Step 4: Metrics (metrics/builder.rs)
- `ws_subscribers_rejected_total` — Counter
- `ws_subscribers_idle_swept_total` — Counter
- `ws_subscribers_active` — Gauge
- `ws_subscribers_per_ip_max` — Gauge (sampled by sweeper)

### Step 5: Unit Tests (wspub.rs)
- `test_connection_guard_decrements_on_drop` — verifies guard cleanup
- `test_per_ip_counter_cleanup_on_zero` — verifies map entry removal
- `test_idle_sweep_closes_inactive_connections` — verifies sweeper fires

## Call Sites Updated
- `crates/builder/src/flashblocks/service.rs` — added per_ip_limit, idle_timeout_secs params
- `crates/builder/src/default/service.rs` — same
- `crates/builder/src/broadcast/mod.rs` — test helper updated

## Decisions Made
- Used `mpsc::channel(1)` instead of `oneshot` for close signaling (allows retry by sweeper)
- Used `try_send` in sweeper to avoid blocking on full channel
- Track per-IP counters even when limit is disabled (for metrics visibility)
- DashMap read → write deadlock avoided by scoping the Ref binding

## Skipped Suggestions
- None

## Test Results
- 3/3 unit tests pass
- clippy clean (zero warnings with -D warnings)
- cargo fmt clean

## Files Changed
- `crates/builder/src/args/op.rs` (+19 lines)
- `crates/builder/src/broadcast/mod.rs` (+2 lines)
- `crates/builder/src/broadcast/wspub.rs` (+311/-31 lines)
- `crates/builder/src/default/service.rs` (+2 lines)
- `crates/builder/src/flashblocks/mod.rs` (+10 lines)
- `crates/builder/src/flashblocks/service.rs` (+2 lines)
- `crates/builder/src/metrics/builder.rs` (+8 lines)
