# PRD: Per-IP WebSocket Subscriber Rate Limit + Idle Sweep (xlayer-reth)

> Source: https://okg-block.sg.larksuite.com/docx/R4EudCU25odwbFx7B80lTYCdgxd
> Last modified: 2026-05-08T03:46:27Z

# 1. Overview

Add per-IP connection caps and an idle-timeout sweep to the flashblocks WebSocket publisher (crates/builder/src/broadcast/wspub.rs). Today the only protection is a single global subscriber count check at wspub.rs:155, gated by the optional ws_subscriber_limit field defined in crates/builder/src/flashblocks/mod.rs:76 and surfaced as a CLI flag in crates/builder/src/args/op.rs:154. A single noisy or buggy client can therefore monopolize the entire global budget, and silent half-open TCP connections accumulate against that budget without ever being reclaimed.

# 2. Goals & Non-Goals

## Goals

- Cap concurrent WebSocket connections per source IP, in addition to the existing global cap.
- Reclaim idle / half-open connections by sweeping subscribers that have not been writable within a configurable timeout.
- Use lock-free atomics for the hot per-IP counter increment / decrement path — no global Mutex on the connection-accept fast path.
- Send a graceful WebSocket Close frame with an explanatory reason when rejecting or sweeping a connection.
- Emit metrics: rejection rate, idle-sweep rate, and active-per-IP distribution.
- Keep the diff small — ~1 file (wspub.rs) plus one CLI flag and one metric counter group.

## Non-Goals

- True token-bucket message-rate limiting per subscriber — separate workstream.
- Authentication or API keys — separate workstream.
- Honoring X-Forwarded-For / proxy headers — follow-up if we end up running behind a reverse proxy that doesn't preserve source IPs (TCP layer here gives us the real peer).
- TLS termination — not in scope for this change.

# 3. Background

WebSocketPublisher::new (wspub.rs:42) opens a single TCP listener and spawns listener_loop, which accepts connections, performs the WebSocket handshake, and spawns a per-connection broadcast_loop (wspub.rs:187). The current admission check is the lone `if let Some(limit) = subscriber_limit && subs.load(Ordering::Relaxed) >= limit` at wspub.rs:155. There is no per-source accounting and no liveness sweep; a connection that goes silent at the TCP layer (NAT timeout, client crash without RST) survives until the next failed send() inside broadcast_loop, which only fires when a flashblock is published. During quiet periods (no flashblocks), dead connections leak indefinitely and consume against the global budget.

Source IP is already available at the listener accept point as `peer_addr: SocketAddr` (wspub.rs:147), so per-IP accounting only needs a shared counter map — no extra plumbing through the handshake.

# 4. Requirements

## Functional

- FR-1: After accept, before incrementing the global subs counter, look up peer_addr.ip() in a per-IP counter; reject with WebSocket Close if the per-IP limit is reached.
- FR-2: Increment the per-IP counter on admission and decrement on broadcast_loop exit (any exit path — idle, error, term, client close).
- FR-3: Track each connection's last successful write timestamp. A periodic sweeper closes connections whose last activity is older than --ws-idle-timeout-secs (default 90s).
- FR-4: When a connection is closed by the sweeper, send a Close frame with code 1001 (Going Away) and reason "idle timeout" before dropping the stream.
- FR-5: Per-IP cap and idle timeout must be configurable via CLI: --ws-per-ip-limit (default 4) and --ws-idle-timeout-secs (default 90). Setting either to 0 disables that check.
- FR-6: All decrements must run in Drop or a final block so that panics inside broadcast_loop do not leak counter capacity.

## Non-Functional

- NFR-1: Per-IP increment / decrement on the accept path must be lock-free (no Mutex held across .await). DashMap entry + AtomicUsize satisfies this.
- NFR-2: Idle sweep must not block the broadcast hot path — it runs as a separate spawned task and only inspects per-connection AtomicU64 timestamps.
- NFR-3: Memory footprint of per-IP map bounded by number of distinct active source IPs; entries are removed on counter → 0.
- NFR-4: No regression in p99 broadcast send latency; verified by the existing wspub stress test.

# 5. Proposed Design

## 5.1 Per-IP Counter

- Introduce `type PerIpCounters = Arc<DashMap<IpAddr, Arc<AtomicUsize>>>`, owned by WebSocketPublisher and cloned into listener_loop and each broadcast_loop.
- On admission: `counters.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0))).fetch_add(1, AcqRel)`. Reject if the post-increment value exceeds limit; on reject, fetch_sub(1) and close.
- On exit: `fetch_sub(1, AcqRel)`; if the resulting value is 0, attempt `counters.remove_if(&ip, |_, c| c.load(Acquire) == 0)` to keep the map small. The remove_if guard prevents racing with a fresh admit.
- Alternative considered: `parking_lot::Mutex<HashMap<IpAddr, usize>>`. Rejected because the accept path is not a hot path today but could become one under attack — lock-free keeps blast radius small.

## 5.2 Idle-Timeout Sweep

- Each broadcast_loop owns an `Arc<AtomicU64>` last_active that records the unix-epoch seconds of the most recent successful stream.send() (after the existing send-error check at wspub.rs:222).
- On admission, push (last_active, close_tx_oneshot) into a `Arc<DashMap<ConnId, ConnHandle>>` registry. ConnId is a u64 monotonic counter — cheap, no IpAddr collision concern.
- WebSocketPublisher::new spawns a sweeper task: `tokio::time::interval(Duration::from_secs(15))` ticks, scans the registry, and for each entry where `now - last_active > timeout`, fires `close_tx_oneshot.send("idle timeout")`.
- The broadcast_loop selects on the close_rx oneshot in addition to its existing arms; on receipt it sends the Close frame and exits, which decrements both subs and the per-IP counter via the existing exit path.
- Sweep cadence (15s) is independent of timeout; with default timeout 90s, worst case a connection sticks around 90+15s before reclaim. Acceptable.
- Note: writes succeed against the kernel send buffer, so a half-open TCP connection may not surface as 'no successful send' for a while. To force an earlier liveness probe, the sweeper also enqueues a Ping frame on tick. The next read in broadcast_loop processes the Pong and refreshes last_active. Stuck Pings show up as failed sends within one or two ticks and exit the loop.

## 5.3 Graceful Close

- Per-IP rejection: CloseCode::Policy (1008) or Again (1013); use Again to suggest the client may retry from a different IP / wait. Reason: "per-ip subscriber limit reached".
- Idle sweep close: CloseCode::Away (1001). Reason: "idle timeout". Followed by stream.close().await with a short bounded timeout (1s) so a wedged stream can't wedge the sweeper.
- Existing global-limit reject path (wspub.rs:155–162) keeps its current behavior — only the per-IP code path is added on top.

## 5.4 Configuration

- Add to args/op.rs alongside ws_subscriber_limit at line 154: `ws_per_ip_limit: Option<u16>` (default Some(4)), `ws_idle_timeout_secs: Option<u32>` (default Some(90)).
- Plumb both fields through flashblocks/mod.rs (line 76 area) and into WebSocketPublisher::new alongside the existing subscriber_limit param.
- Setting either to None or 0 disables that feature; document this in CLI help text.

# 6. Metrics & Observability

- `ws_subscribers_rejected_total{reason=per_ip|global}` — counter.
- `ws_subscribers_idle_swept_total` — counter.
- `ws_subscribers_active` — gauge (replaces / supplements the existing AtomicUsize subs counter).
- `ws_subscribers_per_ip_max` — gauge sampled by the sweeper, captures the highest count of any single IP. Avoid emitting per-IP labels (cardinality risk).
- Log `warn!` once per minute (rate-limited) when an IP repeatedly hits its cap; include the IP only in the log line, never as a metric label.

# 7. Risks & Mitigations

- Risk: NAT / corporate proxy clients share an IP and get unfairly throttled. Mitigation: default cap of 4 is generous; operators with known proxy fronting can raise via flag or set 0 to disable.
- Risk: IPv6 client rotates source addresses (privacy extensions) and bypasses the per-IP cap. Mitigation: the global cap still applies; out of scope to dedup by /64 prefix in this PRD.
- Risk: idle Ping frames consume bandwidth. Mitigation: 15s cadence and small frame size; negligible vs flashblock payloads.
- Risk: sweeper races a freshly-admitted connection of the same IP. Mitigation: AtomicUsize fetch_add returns the post-increment value; reject decision is taken atomically before the connection is registered with the sweeper.
- Risk: panic in broadcast_loop leaks counters. Mitigation: wrap admission/exit in a guard struct whose Drop impl always decrements both counters.

# 8. Test Plan

- Unit: PerIpCounters increment / decrement / remove_if races (loom test or property-style).
- Unit: idle-sweep timer uses tokio::time::pause() to drive virtual time; assert connection closes after timeout + 1 tick.
- Unit (loopback): bind WebSocketPublisher to 127.0.0.1:0, open N+1 connections from the test, with per-ip-limit=N — the (N+1)th must receive a Close frame with reason "per-ip subscriber limit reached" and not count against subs.
- Unit (loopback + virtual time): open a connection, stop reading on the client side, drive tokio::time::pause() and tokio::time::advance() past the idle timeout — server must emit Close 1001 "idle timeout" within one sweep tick, and the per-IP counter must drop to 0.
- Unit: panic injected inside broadcast_loop — counters return to 0 via the Drop guard. Use std::panic::catch_unwind or tokio::task::JoinHandle abort semantics to verify.
- Unit (stress, marked #[ignore] for default runs): spawn 200 concurrent loopback connections from 5 simulated source IPs (127.0.0.1…127.0.0.5) with per-ip-limit=50, publish 1000 flashblocks through the pipe, then close all clients. Assert: (a) no rejected connection within the cap, (b) per-IP counters all return to 0, (c) global subs returns to 0, (d) sweeper task is still alive. Replaces what would have been a devnet soak.

# 9. Rollout Plan

1. Land code with conservative defaults (per-ip 4, idle 90s) but feature off by default (per_ip_limit=None) for first rollout.
2. Verify via the expanded loopback unit-test suite (unit + virtual-time + stress). No devnet stage — loopback tests cover the same surface (admission, idle sweep, graceful close, counter accounting) without requiring a deployed environment.
3. Enable on testnet at default limits.
4. Enable on mainnet replicas; flip default to Some(4).

# 10. Decisions

- DECIDED — Use the raw IpAddr (peer_addr.ip()) as the per-IP key. Do NOT collapse IPv6 to /64. Rationale: the simplest impl is one DashMap<IpAddr, ...> keyed exactly by what accept gives us — zero key-derivation logic. We have no current evidence of IPv6 address-rotation abuse; the global subscriber cap still backstops any rotation attack. If we later observe abuse in ws_subscribers_per_ip_max metrics, swap the key type to a small IpKey enum that masks V6 to /64 — a one-line change isolated to the lookup site.

- DECIDED — Use CloseCode::Again (1013, Try Again Later) for both the per-IP and global cap rejections. Rationale: the cap is a transient capacity limit, not a policy violation — the client may have a legitimate need for multiple subscriptions. CloseCode::Again is the standards-correct signal for "server overloaded, retry later" and matches the existing global-limit reject path at wspub.rs:158. CloseCode::Policy (1008) would be appropriate only for clients we want to permanently reject (auth fail, malformed handshake), which is not what's happening here.

- DECIDED — Send periodic application-level Pings from the sweeper task. Rationale: relying on stream.send() to surface dead connections is unreliable — writes succeed against the kernel TCP buffer until it fills, so a frozen client whose kernel still ACKs never triggers a send error and last_active keeps refreshing. WebSocket Pings force a Pong round-trip; a missing Pong + the existing idle timeout reliably reaps zombie connections. Implementation stays simple: the sweeper task already iterates the connection registry every 15s; it just enqueues a Ping frame on each tick via the same close_tx channel pattern (extended to a small enum SweeperMsg { Ping, Close(reason) }). tokio_tungstenite auto-replies to incoming Pings, so client-side requires no changes.
