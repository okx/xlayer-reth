# IMPL: Per-IP WebSocket Subscriber Rate Limit + Idle Sweep

## Background

The flashblocks WebSocket publisher (`crates/builder/src/broadcast/wspub.rs`) currently has only a global subscriber cap. This change adds per-IP connection limits and an idle-timeout sweep to prevent resource exhaustion from noisy/dead clients.

## Objectives

1. Add per-IP connection caps via lock-free `DashMap<IpAddr, Arc<AtomicUsize>>`
2. Add idle-timeout sweep that closes connections not active within a configurable window
3. Add CLI flags `--flashblocks.ws-per-ip-limit` and `--flashblocks.ws-idle-timeout-secs`
4. Add metrics for rejection and sweep rates
5. Ensure counter safety via Drop guard

## Step 1: Add CLI Arguments and Configuration Fields

### Modify: `crates/builder/src/args/op.rs`

Add after `ws_subscriber_limit` (line 154):

```rust
/// Maximum number of concurrent WebSocket connections per IP address.
/// Set to 0 to disable per-IP limiting.
#[arg(
    long = "flashblocks.ws-per-ip-limit",
    env = "FLASHBLOCK_WS_PER_IP_LIMIT",
    default_value = "4"
)]
pub ws_per_ip_limit: Option<u16>,

/// Idle timeout in seconds for WebSocket connections.
/// Connections with no successful write for this duration will be closed.
/// Set to 0 to disable idle sweep.
#[arg(
    long = "flashblocks.ws-idle-timeout-secs",
    env = "FLASHBLOCK_WS_IDLE_TIMEOUT_SECS",
    default_value = "90"
)]
pub ws_idle_timeout_secs: Option<u32>,
```

### Modify: `crates/builder/src/flashblocks/mod.rs`

Add to `FlashblocksConfig` struct after `ws_subscriber_limit`:
```rust
pub ws_per_ip_limit: Option<u16>,
pub ws_idle_timeout_secs: Option<u32>,
```

Update `Default` impl and `TryFrom<BuilderArgs>` to plumb the new fields.

## Step 2: Implement Per-IP Counter and Connection Guard

### Modify: `crates/builder/src/broadcast/wspub.rs`

Add types:
```rust
use std::net::IpAddr;
use std::sync::atomic::AtomicU64;
use dashmap::DashMap;
```

Add `PerIpCounters` type alias and `ConnectionGuard` struct:
- `type PerIpCounters = Arc<DashMap<IpAddr, Arc<AtomicUsize>>>`
- `ConnectionGuard` with `Drop` that decrements both global subs and per-IP counter

Update `WebSocketPublisher::new` signature to accept `ws_per_ip_limit` and `ws_idle_timeout_secs`.

Update `listener_loop` to:
1. Check per-IP limit before admitting connection
2. Wrap admission in `ConnectionGuard`
3. Track `last_active` timestamp per connection

## Step 3: Implement Idle-Timeout Sweep

### Modify: `crates/builder/src/broadcast/wspub.rs`

Add:
- `ConnId` (u64 monotonic counter)
- `ConnHandle` struct holding `last_active: Arc<AtomicU64>` and `close_tx: oneshot::Sender<&'static str>`
- Connection registry: `Arc<DashMap<u64, ConnHandle>>`
- Sweeper task: spawned in `WebSocketPublisher::new`, ticks every 15s, closes idle connections
- `broadcast_loop` extended to select on close_rx oneshot

## Step 4: Add Metrics

### Modify: `crates/builder/src/metrics/builder.rs`

Add counters/gauges:
```rust
pub ws_subscribers_rejected_total: Counter,
pub ws_subscribers_idle_swept_total: Counter,
pub ws_subscribers_active: Gauge,
pub ws_subscribers_per_ip_max: Gauge,
```

## Step 5: Write Unit Tests

### New file: `crates/builder/src/broadcast/wspub_test.rs`

Tests:
- `test_per_ip_limit_rejects_excess_connections` — open N+1 connections from same IP with limit N
- `test_idle_sweep_closes_inactive_connections` — use `tokio::time::pause()` to drive past timeout
- `test_connection_guard_decrements_on_drop` — verify counter cleanup
- `test_per_ip_counter_cleanup_on_zero` — verify map entry removal

## UT/IT Matrix

| Step | Test | Type |
|------|------|------|
| Step 2 | test_per_ip_limit_rejects_excess_connections | UT (loopback) |
| Step 2 | test_connection_guard_decrements_on_drop | UT |
| Step 3 | test_idle_sweep_closes_inactive_connections | UT (virtual time) |
| Step 2 | test_per_ip_counter_cleanup_on_zero | UT |

## Checklist Updates

- Add: "Per-IP WebSocket rate limiting verified with loopback tests"
- Add: "Idle sweep tested with virtual time advancement"
