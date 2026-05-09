# IMPL: Per-IP WebSocket Subscriber Rate Limit + Idle Sweep

## Background
Add per-IP connection caps and idle-timeout sweep to the flashblocks WebSocket publisher (wspub.rs). Currently only a global subscriber limit exists. We need per-IP throttling and idle connection cleanup.

## Step 1: Add CLI Args & Config Fields

### Modify: `crates/builder/src/args/op.rs`
Add two new fields to `FlashblocksArgs` after `ws_subscriber_limit` (line 154):
- `ws_per_ip_limit: Option<u16>` — CLI flag `--flashblocks.ws-per-ip-limit`, env `FLASHBLOCK_WS_PER_IP_LIMIT`, default "4"
- `ws_idle_timeout_secs: Option<u32>` — CLI flag `--flashblocks.ws-idle-timeout-secs`, env `FLASHBLOCK_WS_IDLE_TIMEOUT_SECS`, default "90"

### Modify: `crates/builder/src/flashblocks/mod.rs`
- Add `ws_per_ip_limit: Option<u16>` and `ws_idle_timeout_secs: Option<u32>` to `FlashblocksConfig` after `ws_subscriber_limit`
- Add defaults: `ws_per_ip_limit: None`, `ws_idle_timeout_secs: None`
- Map from args at the config construction site (~line 217)

## Step 2: Implement Per-IP Counter + Idle Sweep + Guard in wspub.rs

### Modify: `crates/builder/src/broadcast/wspub.rs`

#### New imports
- `dashmap::DashMap`
- `std::net::IpAddr`
- `std::sync::atomic::AtomicU64`
- `std::time::{SystemTime, UNIX_EPOCH}`
- `tokio::sync::mpsc`
- `tokio::time::{interval, Duration}`

#### New types/structs
- `type PerIpCounters = Arc<DashMap<IpAddr, Arc<AtomicUsize>>>`
- `struct ConnHandle { last_active: Arc<AtomicU64>, close_tx: mpsc::Sender<&'static str> }`
- `type ConnRegistry = Arc<DashMap<u64, ConnHandle>>`
- `struct SubscriberGuard` — Drop decrements per-IP counter and global subs, removes from conn registry

#### Modify `WebSocketPublisher`
- Add fields: `per_ip_counters: PerIpCounters`, `conn_registry: ConnRegistry`, `per_ip_limit: Option<u16>`, `idle_timeout_secs: Option<u32>`
- Modify `new()` signature: add `per_ip_limit: Option<u16>`, `idle_timeout_secs: Option<u32>`
- In `new()`: create PerIpCounters and ConnRegistry, pass to listener_loop, spawn sweeper task

#### Modify `listener_loop`
- Add params: `per_ip_counters`, `conn_registry`, `per_ip_limit`, `idle_timeout_secs`
- After accept_async succeeds, before global limit check: per-IP check
- Create SubscriberGuard on admission
- Pass guard + conn_registry + last_active to broadcast_loop

#### Modify `broadcast_loop`
- Add params: `guard: SubscriberGuard`, `close_rx: mpsc::Receiver<&'static str>`, `last_active: Arc<AtomicU64>`
- Update `last_active` after each successful send
- Add select arm for `close_rx.recv()` → send Close frame and return

#### New function `sweeper_loop`
- Runs on a 15s interval
- Iterates conn_registry, checks `now - last_active > timeout`
- Sends "idle timeout" via close_tx for expired connections
- Updates metrics (idle sweep counter, per-IP max gauge)

## Step 3: Add Metrics

### Modify: `crates/builder/src/metrics/builder.rs`
Add to `BuilderMetrics`:
- `ws_subscribers_rejected_per_ip: Counter`
- `ws_subscribers_rejected_global: Counter`
- `ws_subscribers_idle_swept: Counter`
- `ws_subscribers_active: Gauge`
- `ws_subscribers_per_ip_max: Gauge`

## Step 4: Update Service Callers

### Modify: `crates/builder/src/default/service.rs` and `crates/builder/src/flashblocks/service.rs`
- Pass `ws_per_ip_limit` and `ws_idle_timeout_secs` from config to `WebSocketPublisher::new()`

## Test Matrix

### Unit Tests (in wspub.rs or new test file)
- `test_per_ip_limit_rejects_excess_connections` — loopback, N+1 connections
- `test_idle_sweep_closes_inactive_connections` — virtual time advance
- `test_subscriber_guard_decrements_on_drop` — verify counters return to 0

All tests use `#[tokio::test]` with loopback binding to `127.0.0.1:0`.
