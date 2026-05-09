use crate::{
    broadcast::XLayerFlashblockMessage, metrics::tokio::MonitoredTask, metrics::BuilderMetrics,
};
use core::{
    fmt::{Debug, Formatter},
    net::SocketAddr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
use dashmap::DashMap;
use futures::SinkExt;
use futures_util::StreamExt;
use std::{
    io,
    net::{IpAddr, TcpListener},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::{
        broadcast::{self, error::RecvError, Receiver},
        mpsc, watch,
    },
    time::{interval, Duration},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        protocol::frame::{coding::CloseCode, CloseFrame},
        Message, Utf8Bytes,
    },
    WebSocketStream,
};
use tracing::{debug, info, trace, warn};

type PerIpCounters = Arc<DashMap<IpAddr, Arc<AtomicUsize>>>;
type ConnRegistry = Arc<DashMap<u64, ConnHandle>>;

struct ConnHandle {
    last_active: Arc<AtomicU64>,
    close_tx: mpsc::Sender<&'static str>,
}

static CONN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Guard that decrements per-IP and global subscriber counters on drop.
struct SubscriberGuard {
    ip: IpAddr,
    conn_id: u64,
    per_ip_counters: PerIpCounters,
    subs: Arc<AtomicUsize>,
    conn_registry: ConnRegistry,
    metrics: Arc<BuilderMetrics>,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.subs.fetch_sub(1, Ordering::AcqRel);
        self.metrics.ws_subscribers_active.decrement(1.0);
        self.conn_registry.remove(&self.conn_id);

        // Must not hold a Ref while calling remove_if (same-shard deadlock).
        let should_remove = self
            .per_ip_counters
            .get(&self.ip)
            .map(|counter| counter.fetch_sub(1, Ordering::AcqRel) <= 1)
            .unwrap_or(false);
        if should_remove {
            self.per_ip_counters.remove_if(&self.ip, |_, c| c.load(Ordering::Acquire) == 0);
        }
    }
}

/// A WebSockets publisher that accepts connections from client websockets and broadcasts to them
/// updates about new flashblocks. It maintains a count of sent messages and active subscriptions.
///
/// This is modelled as a `futures::Sink` that can be used to send `OpFlashblockPayload` messages.
pub struct WebSocketPublisher {
    sent: Arc<AtomicUsize>,
    subs: Arc<AtomicUsize>,
    term: watch::Sender<bool>,
    pipe: broadcast::Sender<Utf8Bytes>,
    subscriber_limit: Option<u16>,
    per_ip_limit: Option<u16>,
    idle_timeout_secs: Option<u32>,
}

impl WebSocketPublisher {
    pub fn new(
        addr: SocketAddr,
        metrics: Arc<BuilderMetrics>,
        task_monitor: &MonitoredTask,
        subscriber_limit: Option<u16>,
        per_ip_limit: Option<u16>,
        idle_timeout_secs: Option<u32>,
    ) -> io::Result<Self> {
        let (pipe, _) = broadcast::channel(100);
        let (term, _) = watch::channel(false);

        let sent = Arc::new(AtomicUsize::new(0));
        let subs = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind(addr)?;

        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let conn_registry: ConnRegistry = Arc::new(DashMap::new());

        // Spawn sweeper task for idle connections
        if let Some(timeout) = idle_timeout_secs
            && timeout > 0
        {
            let registry = Arc::clone(&conn_registry);
            let sweep_metrics = Arc::clone(&metrics);
            let sweep_per_ip = Arc::clone(&per_ip_counters);
            tokio::spawn(sweeper_loop(registry, sweep_metrics, sweep_per_ip, timeout));
        }

        tokio::spawn(task_monitor.instrument(listener_loop(
            listener,
            metrics,
            pipe.subscribe(),
            term.subscribe(),
            Arc::clone(&sent),
            Arc::clone(&subs),
            subscriber_limit,
            per_ip_limit,
            per_ip_counters,
            conn_registry,
        )));

        Ok(Self { sent, subs, term, pipe, subscriber_limit, per_ip_limit, idle_timeout_secs })
    }

    pub fn publish(&self, payload: &XLayerFlashblockMessage) -> io::Result<usize> {
        match payload {
            XLayerFlashblockMessage::Payload(payload) => {
                info!(
                    target: "payload_builder::broadcast",
                    event = "flashblock_sent",
                    message = "Sending flashblock to subscribers",
                    id = %payload.inner.payload_id,
                    index = payload.inner.index,
                    base = payload.inner.base.is_some(),
                    target_index = payload.target_index,
                );
            }
            XLayerFlashblockMessage::PayloadEnd(payload) => {
                info!(
                    target: "payload_builder::broadcast",
                    event = "flashblock_end_sent",
                    message = "Sending flashblock to subscribers",
                    id = %payload.payload_id,
                );
            }
        }

        let serialized = serde_json::to_string(payload)?;
        let utf8_bytes = Utf8Bytes::from(serialized);
        let size = utf8_bytes.len();
        self.pipe
            .send(utf8_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e))?;
        Ok(size)
    }
}

impl Drop for WebSocketPublisher {
    fn drop(&mut self) {
        let _ = self.term.send(true);
        info!(target: "payload_builder::broadcast", "WebSocketPublisher dropped, terminating listener loop");
    }
}

#[allow(clippy::too_many_arguments)]
async fn listener_loop(
    listener: TcpListener,
    metrics: Arc<BuilderMetrics>,
    receiver: Receiver<Utf8Bytes>,
    term: watch::Receiver<bool>,
    sent: Arc<AtomicUsize>,
    subs: Arc<AtomicUsize>,
    subscriber_limit: Option<u16>,
    per_ip_limit: Option<u16>,
    per_ip_counters: PerIpCounters,
    conn_registry: ConnRegistry,
) {
    listener.set_nonblocking(true).expect("Failed to set TcpListener socket to non-blocking");

    let listener = tokio::net::TcpListener::from_std(listener)
        .expect("Failed to convert TcpListener to tokio TcpListener");

    let listen_addr = listener.local_addr().expect("Failed to get local address of listener");
    info!(target: "payload_builder::broadcast", "Flashblocks WebSocketPublisher listening on {listen_addr}");

    let mut term = term;

    loop {
        let subs = Arc::clone(&subs);
        let metrics = Arc::clone(&metrics);

        tokio::select! {
            _ = term.changed() => {
                if *term.borrow() {
                    return;
                }
            }

            Ok((connection, peer_addr)) = listener.accept() => {
                let sent = Arc::clone(&sent);
                let term = term.clone();
                let receiver_clone = receiver.resubscribe();
                let per_ip_counters = Arc::clone(&per_ip_counters);
                let conn_registry = Arc::clone(&conn_registry);
                let metrics = Arc::clone(&metrics);

                match accept_async(connection).await {
                    Ok(mut stream) => {
                        tokio::spawn(async move {
                            let ip = peer_addr.ip();

                            // Per-IP limit check
                            if let Some(limit) = per_ip_limit
                                && limit > 0
                            {
                                let counter = per_ip_counters
                                    .entry(ip)
                                    .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
                                    .clone();
                                let current = counter.fetch_add(1, Ordering::AcqRel);
                                if current >= limit as usize {
                                    counter.fetch_sub(1, Ordering::AcqRel);
                                    warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: per-ip limit reached");
                                    metrics.ws_subscribers_rejected_per_ip.increment(1);
                                    let _ = stream.close(Some(CloseFrame {
                                        code: CloseCode::Again,
                                        reason: "per-ip subscriber limit reached".into(),
                                    })).await;
                                    return;
                                }
                            }

                            // Global limit check
                            if let Some(limit) = subscriber_limit
                                && subs.load(Ordering::Relaxed) >= limit as usize
                            {
                                // Undo per-IP increment (avoid holding Ref across remove_if)
                                if let Some(ip_limit) = per_ip_limit
                                    && ip_limit > 0
                                {
                                    let should_remove = per_ip_counters
                                        .get(&ip)
                                        .map(|c| c.fetch_sub(1, Ordering::AcqRel) <= 1)
                                        .unwrap_or(false);
                                    if should_remove {
                                        per_ip_counters.remove_if(&ip, |_, c| c.load(Ordering::Acquire) == 0);
                                    }
                                }
                                warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: subscriber limit reached");
                                metrics.ws_subscribers_rejected_global.increment(1);
                                let _ = stream.close(Some(CloseFrame {
                                    code: CloseCode::Again,
                                    reason: "subscriber limit reached, please try again later".into(),
                                })).await;
                                return;
                            }

                            subs.fetch_add(1, Ordering::AcqRel);
                            metrics.ws_subscribers_active.increment(1.0);
                            debug!(target: "payload_builder::broadcast", "WebSocket connection established with {}", peer_addr);

                            // Set up connection handle for sweeper
                            let last_active = Arc::new(AtomicU64::new(now_secs()));
                            let (close_tx, close_rx) = mpsc::channel(1);
                            let conn_id = CONN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

                            conn_registry.insert(conn_id, ConnHandle {
                                last_active: Arc::clone(&last_active),
                                close_tx,
                            });

                            let guard = SubscriberGuard {
                                ip,
                                conn_id,
                                per_ip_counters,
                                subs,
                                conn_registry,
                                metrics,
                            };

                            broadcast_loop(stream, term, receiver_clone, sent, last_active, close_rx).await;

                            drop(guard);
                            debug!(target: "payload_builder::broadcast", "WebSocket connection closed for {}", peer_addr);
                        });
                    }
                    Err(e) => {
                        warn!(target: "payload_builder::broadcast", "Failed to accept WebSocket connection from {peer_addr}: {e}");
                    }
                }
            }
        }
    }
}

async fn broadcast_loop(
    stream: WebSocketStream<TcpStream>,
    term: watch::Receiver<bool>,
    blocks: broadcast::Receiver<Utf8Bytes>,
    sent: Arc<AtomicUsize>,
    last_active: Arc<AtomicU64>,
    mut close_rx: mpsc::Receiver<&'static str>,
) {
    let mut term = term;
    let mut blocks = blocks;
    let mut stream = stream;
    let Ok(peer_addr) = stream.get_ref().peer_addr() else {
        return;
    };

    loop {
        tokio::select! {
            _ = term.changed() => {
                if *term.borrow() {
                    info!(target: "payload_builder::broadcast", "WebSocketPublisher is terminating, closing broadcast loop");
                    return;
                }
            }

            payload = blocks.recv() => match payload {
                Ok(payload) => {
                    sent.fetch_add(1, Ordering::Relaxed);

                    trace!(target: "payload_builder::broadcast", "Broadcasted payload: {:?}", payload);
                    if let Err(e) = stream.send(Message::Text(payload)).await {
                        debug!(target: "payload_builder::broadcast", "Send payload error for flashblocks subscription {peer_addr}: {e}");
                        break;
                    }
                    last_active.store(now_secs(), Ordering::Release);
                }
                Err(RecvError::Closed) => {
                    debug!(target: "payload_builder::broadcast", "Broadcast channel closed, exiting broadcast loop");
                    return;
                }
                Err(RecvError::Lagged(_)) => {
                    warn!(target: "payload_builder::broadcast", "Broadcast channel lagged, some messages were dropped");
                }
            },

            message = stream.next() => if let Some(message) = message { match message {
                Ok(Message::Close(_)) => {
                    info!(target: "payload_builder::broadcast", "Closing frame received, stopping connection for {peer_addr}");
                    break;
                }
                Ok(Message::Pong(_)) => {
                    last_active.store(now_secs(), Ordering::Release);
                }
                Err(e) => {
                    warn!(target: "payload_builder::broadcast", "Received error. Closing flashblocks subscription for {peer_addr}: {e}");
                    break;
                }
                _ => (),
            } },

            reason = close_rx.recv() => {
                let reason = reason.unwrap_or("idle timeout");
                debug!(target: "payload_builder::broadcast", "Sweeper closing connection for {peer_addr}: {reason}");
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    stream.close(Some(CloseFrame {
                        code: CloseCode::Away,
                        reason: reason.into(),
                    }))
                ).await;
                break;
            }
        }
    }
}

async fn sweeper_loop(
    conn_registry: ConnRegistry,
    metrics: Arc<BuilderMetrics>,
    per_ip_counters: PerIpCounters,
    timeout_secs: u32,
) {
    let mut tick = interval(Duration::from_secs(15));
    tick.tick().await; // first tick fires immediately, skip it

    loop {
        tick.tick().await;
        let now = now_secs();
        let mut max_per_ip: usize = 0;

        // Scan for idle connections
        let mut to_close = Vec::new();
        for entry in conn_registry.iter() {
            let last = entry.value().last_active.load(Ordering::Acquire);
            if now.saturating_sub(last) > timeout_secs as u64 {
                to_close.push(entry.key().to_owned());
            }
        }

        for conn_id in to_close {
            if let Some((_, handle)) = conn_registry.remove(&conn_id) {
                let _ = handle.close_tx.try_send("idle timeout");
                metrics.ws_subscribers_idle_swept.increment(1);
            }
        }

        // Update per-IP max metric
        for entry in per_ip_counters.iter() {
            let count = entry.value().load(Ordering::Relaxed);
            if count > max_per_ip {
                max_per_ip = count;
            }
        }
        metrics.ws_subscribers_per_ip_max.set(max_per_ip as f64);
    }
}

impl Debug for WebSocketPublisher {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let subs = self.subs.load(Ordering::Relaxed);
        let sent = self.sent.load(Ordering::Relaxed);
        let subscriber_limit = self.subscriber_limit;

        f.debug_struct("WebSocketPublisher")
            .field("subs", &subs)
            .field("payloads_sent", &sent)
            .field("subscriber_limit", &subscriber_limit)
            .field("per_ip_limit", &self.per_ip_limit)
            .field("idle_timeout_secs", &self.idle_timeout_secs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_subscriber_guard_decrements_on_drop() {
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let subs = Arc::new(AtomicUsize::new(1));
        let conn_registry: ConnRegistry = Arc::new(DashMap::new());
        let metrics = Arc::new(BuilderMetrics::default());
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Set up per-IP counter
        per_ip_counters.insert(ip, Arc::new(AtomicUsize::new(1)));

        // Insert a dummy conn handle
        let (close_tx, _close_rx) = mpsc::channel(1);
        conn_registry.insert(
            42,
            ConnHandle { last_active: Arc::new(AtomicU64::new(now_secs())), close_tx },
        );

        let guard = SubscriberGuard {
            ip,
            conn_id: 42,
            per_ip_counters: Arc::clone(&per_ip_counters),
            subs: Arc::clone(&subs),
            conn_registry: Arc::clone(&conn_registry),
            metrics,
        };

        // Drop the guard
        drop(guard);

        // Verify all counters decremented
        assert_eq!(subs.load(Ordering::Relaxed), 0);
        assert!(conn_registry.is_empty());
        // Per-IP entry should be removed since count went to 0
        assert!(per_ip_counters.get(&ip).is_none());
    }

    #[tokio::test]
    async fn test_per_ip_counter_increment_decrement() {
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        // Increment
        let counter =
            per_ip_counters.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0))).clone();
        let val = counter.fetch_add(1, Ordering::AcqRel);
        assert_eq!(val, 0);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Increment again
        let val = counter.fetch_add(1, Ordering::AcqRel);
        assert_eq!(val, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // Decrement
        let prev = counter.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(prev, 2);

        // Decrement to zero and remove
        let prev = counter.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(prev, 1);
        if prev <= 1 {
            per_ip_counters.remove_if(&ip, |_, c| c.load(Ordering::Acquire) == 0);
        }
        assert!(per_ip_counters.get(&ip).is_none());
    }

    #[tokio::test]
    async fn test_per_ip_limit_rejects_excess_connections() {
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let limit: u16 = 2;

        // Simulate 2 connections from same IP (admission logic)
        let counter =
            per_ip_counters.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0))).clone();
        let current = counter.fetch_add(1, Ordering::AcqRel);
        assert!(current < limit as usize); // conn 1 admitted
        let current = counter.fetch_add(1, Ordering::AcqRel);
        assert!(current < limit as usize); // conn 2 admitted

        // Third connection should be rejected (current == 2 >= limit)
        let current = counter.fetch_add(1, Ordering::AcqRel);
        assert!(current >= limit as usize);
        counter.fetch_sub(1, Ordering::AcqRel); // undo on reject

        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_sweeper_identifies_idle_connections() {
        let conn_registry: ConnRegistry = Arc::new(DashMap::new());
        let metrics = Arc::new(BuilderMetrics::default());
        let _per_ip_counters: PerIpCounters = Arc::new(DashMap::new());

        // Insert a connection that is already expired (last_active in the past)
        let (close_tx, mut close_rx) = mpsc::channel(1);
        let expired_time = now_secs().saturating_sub(100); // 100 seconds ago
        conn_registry
            .insert(1, ConnHandle { last_active: Arc::new(AtomicU64::new(expired_time)), close_tx });

        // Insert a connection that is still active
        let (close_tx2, mut close_rx2) = mpsc::channel(1);
        conn_registry.insert(
            2,
            ConnHandle { last_active: Arc::new(AtomicU64::new(now_secs())), close_tx: close_tx2 },
        );

        // Simulate one sweep iteration (timeout = 90s)
        let now = now_secs();
        let timeout_secs: u32 = 90;
        let mut to_close = Vec::new();
        for entry in conn_registry.iter() {
            let last = entry.value().last_active.load(Ordering::Acquire);
            if now.saturating_sub(last) > timeout_secs as u64 {
                to_close.push(*entry.key());
            }
        }

        for conn_id in to_close {
            if let Some((_, handle)) = conn_registry.remove(&conn_id) {
                let _ = handle.close_tx.try_send("idle timeout");
                metrics.ws_subscribers_idle_swept.increment(1);
            }
        }

        // Expired connection should have received close signal
        let msg = close_rx.try_recv();
        assert_eq!(msg, Ok("idle timeout"));

        // Active connection should not have received close signal
        let msg2 = close_rx2.try_recv();
        assert!(msg2.is_err());

        // Only the active connection should remain in registry
        assert_eq!(conn_registry.len(), 1);
        assert!(conn_registry.contains_key(&2));
    }
}
