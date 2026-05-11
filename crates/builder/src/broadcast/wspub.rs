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
use std::{io, net::IpAddr, net::TcpListener, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{
        broadcast::{self, error::RecvError, Receiver},
        mpsc, watch,
    },
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

/// Handle held by the sweeper to signal a connection to close.
struct ConnHandle {
    last_active: Arc<AtomicU64>,
    close_tx: mpsc::Sender<&'static str>,
    ip: IpAddr,
}

/// Guard that decrements both global subs and per-IP counter on drop.
struct ConnectionGuard {
    subs: Arc<AtomicUsize>,
    per_ip_counters: PerIpCounters,
    ip: IpAddr,
    conn_id: u64,
    registry: Arc<DashMap<u64, ConnHandle>>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.subs.fetch_sub(1, Ordering::AcqRel);

        let should_remove = self.per_ip_counters.get(&self.ip).map(|counter| {
            let prev = counter.fetch_sub(1, Ordering::AcqRel);
            prev == 1
        });
        // Ref is dropped here, releasing the read lock
        if should_remove == Some(true) {
            self.per_ip_counters.remove_if(&self.ip, |_, c| c.load(Ordering::Acquire) == 0);
        }

        self.registry.remove(&self.conn_id);
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
        let conn_registry: Arc<DashMap<u64, ConnHandle>> = Arc::new(DashMap::new());

        let effective_idle_timeout = idle_timeout_secs.and_then(|s| {
            if s == 0 {
                None
            } else {
                Some(Duration::from_secs(s as u64))
            }
        });

        if let Some(timeout) = effective_idle_timeout {
            let registry = Arc::clone(&conn_registry);
            let sweep_metrics = Arc::clone(&metrics);
            tokio::spawn(idle_sweep_loop(registry, timeout, sweep_metrics));
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
            effective_idle_timeout.is_some(),
        )));

        Ok(Self { sent, subs, term, pipe, subscriber_limit })
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

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(0);

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
    conn_registry: Arc<DashMap<u64, ConnHandle>>,
    track_activity: bool,
) {
    listener.set_nonblocking(true).expect("Failed to set TcpListener socket to non-blocking");

    let listener = tokio::net::TcpListener::from_std(listener)
        .expect("Failed to convert TcpListener to tokio TcpListener");

    let listen_addr = listener.local_addr().expect("Failed to get local address of listener");
    info!(target: "payload_builder::broadcast", "Flashblocks WebSocketPublisher listening on {listen_addr}");

    let mut term = term;

    let effective_per_ip_limit = per_ip_limit.and_then(|l| if l == 0 { None } else { Some(l) });

    loop {
        let subs = Arc::clone(&subs);
        let metrics = Arc::clone(&metrics);
        let per_ip_counters = Arc::clone(&per_ip_counters);
        let conn_registry = Arc::clone(&conn_registry);

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
                let per_ip_counters = per_ip_counters.clone();
                let conn_registry = conn_registry.clone();

                match accept_async(connection).await {
                    Ok(mut stream) => {
                        tokio::spawn(async move {
                            // Global subscriber limit check
                            if let Some(limit) = subscriber_limit
                                && subs.load(Ordering::Relaxed) >= limit as usize
                            {
                                warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: subscriber limit reached");
                                metrics.ws_subscribers_rejected_total.increment(1);
                                let _ = stream.close(Some(CloseFrame {
                                    code: CloseCode::Again,
                                    reason: "subscriber limit reached, please try again later".into(),
                                })).await;
                                return;
                            }

                            let ip = peer_addr.ip();

                            // Per-IP limit check
                            if let Some(limit) = effective_per_ip_limit {
                                let counter = per_ip_counters
                                    .entry(ip)
                                    .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
                                    .clone();
                                let current = counter.fetch_add(1, Ordering::AcqRel);
                                if current >= limit as usize {
                                    counter.fetch_sub(1, Ordering::AcqRel);
                                    warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: per-ip subscriber limit reached");
                                    metrics.ws_subscribers_rejected_total.increment(1);
                                    let _ = stream.close(Some(CloseFrame {
                                        code: CloseCode::Again,
                                        reason: "per-ip subscriber limit reached".into(),
                                    })).await;
                                    return;
                                }
                            } else {
                                // Still track the counter even if no limit
                                per_ip_counters
                                    .entry(ip)
                                    .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
                                    .fetch_add(1, Ordering::AcqRel);
                            }

                            subs.fetch_add(1, Ordering::AcqRel);
                            metrics.ws_subscribers_active.set(subs.load(Ordering::Relaxed) as f64);
                            debug!(target: "payload_builder::broadcast", "WebSocket connection established with {}", peer_addr);

                            let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                            let (close_tx, close_rx) = mpsc::channel(1);

                            let last_active = Arc::new(AtomicU64::new(now_secs()));

                            if track_activity {
                                conn_registry.insert(conn_id, ConnHandle {
                                    last_active: Arc::clone(&last_active),
                                    close_tx,
                                    ip,
                                });
                            }

                            let guard = ConnectionGuard {
                                subs: Arc::clone(&subs),
                                per_ip_counters,
                                ip,
                                conn_id,
                                registry: conn_registry,
                            };

                            broadcast_loop(stream, metrics.clone(), term, receiver_clone, sent, close_rx, last_active, track_activity).await;

                            drop(guard);
                            metrics.ws_subscribers_active.set(subs.load(Ordering::Relaxed) as f64);
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

async fn idle_sweep_loop(
    registry: Arc<DashMap<u64, ConnHandle>>,
    timeout: Duration,
    metrics: Arc<BuilderMetrics>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    interval.tick().await; // first tick is immediate

    loop {
        interval.tick().await;

        let now = now_secs();
        let timeout_secs = timeout.as_secs();
        let mut max_per_ip: usize = 0;
        let mut swept = 0u64;

        // Track per-IP counts for the max gauge
        let mut ip_counts: std::collections::HashMap<IpAddr, usize> =
            std::collections::HashMap::new();

        for entry in registry.iter() {
            let handle = entry.value();
            let last = handle.last_active.load(Ordering::Acquire);
            *ip_counts.entry(handle.ip).or_default() += 1;

            if now.saturating_sub(last) > timeout_secs {
                let _ = handle.close_tx.try_send("idle timeout");
                swept += 1;
            }
        }

        if swept > 0 {
            metrics.ws_subscribers_idle_swept_total.increment(swept);
        }

        for count in ip_counts.values() {
            if *count > max_per_ip {
                max_per_ip = *count;
            }
        }
        metrics.ws_subscribers_per_ip_max.set(max_per_ip as f64);
    }
}

#[allow(clippy::too_many_arguments)]
async fn broadcast_loop(
    stream: WebSocketStream<TcpStream>,
    metrics: Arc<BuilderMetrics>,
    term: watch::Receiver<bool>,
    blocks: broadcast::Receiver<Utf8Bytes>,
    sent: Arc<AtomicUsize>,
    mut close_rx: mpsc::Receiver<&'static str>,
    last_active: Arc<AtomicU64>,
    track_activity: bool,
) {
    let mut term = term;
    let mut blocks = blocks;
    let mut stream = stream;
    let Ok(peer_addr) = stream.get_ref().peer_addr() else {
        return;
    };

    loop {
        let metrics = Arc::clone(&metrics);

        tokio::select! {
            _ = term.changed() => {
                if *term.borrow() {
                    info!(target: "payload_builder::broadcast", "WebSocketPublisher is terminating, closing broadcast loop");
                    return;
                }
            }

            Some(reason) = close_rx.recv() => {
                info!(target: "payload_builder::broadcast", "Closing connection for {peer_addr}: {reason}");
                let close_fut = stream.close(Some(CloseFrame {
                    code: CloseCode::Away,
                    reason: reason.into(),
                }));
                let _ = tokio::time::timeout(Duration::from_secs(1), close_fut).await;
                return;
            }

            payload = blocks.recv() => match payload {
                Ok(payload) => {
                    sent.fetch_add(1, Ordering::Relaxed);
                    metrics.messages_sent_count.increment(1);

                    trace!(target: "payload_builder::broadcast", "Broadcasted payload: {:?}", payload);
                    if let Err(e) = stream.send(Message::Text(payload)).await {
                        debug!(target: "payload_builder::broadcast", "Send payload error for flashblocks subscription {peer_addr}: {e}");
                        break;
                    }
                    if track_activity {
                        last_active.store(now_secs(), Ordering::Release);
                    }
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
                Err(e) => {
                    warn!(target: "payload_builder::broadcast", "Received error. Closing flashblocks subscription for {peer_addr}: {e}");
                    break;
                }
                _ => (),
            } }
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
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
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_guard_decrements_on_drop() {
        let subs = Arc::new(AtomicUsize::new(1));
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let registry: Arc<DashMap<u64, ConnHandle>> = Arc::new(DashMap::new());

        per_ip_counters.insert(ip, Arc::new(AtomicUsize::new(1)));

        let guard = ConnectionGuard {
            subs: Arc::clone(&subs),
            per_ip_counters: per_ip_counters.clone(),
            ip,
            conn_id: 42,
            registry: registry.clone(),
        };

        drop(guard);

        assert_eq!(subs.load(Ordering::Relaxed), 0);
        // Entry should be removed since counter went to 0
        assert!(per_ip_counters.get(&ip).is_none());
    }

    #[tokio::test]
    async fn test_per_ip_counter_cleanup_on_zero() {
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        // Simulate two connections
        let counter =
            per_ip_counters.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0))).clone();
        counter.fetch_add(1, Ordering::AcqRel);
        counter.fetch_add(1, Ordering::AcqRel);
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // First disconnect
        counter.fetch_sub(1, Ordering::AcqRel);
        assert!(per_ip_counters.get(&ip).is_some());

        // Second disconnect
        let prev = counter.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            per_ip_counters.remove_if(&ip, |_, c| c.load(Ordering::Acquire) == 0);
        }
        assert!(per_ip_counters.get(&ip).is_none());
    }

    #[tokio::test]
    async fn test_idle_sweep_closes_inactive_connections() {
        let registry: Arc<DashMap<u64, ConnHandle>> = Arc::new(DashMap::new());
        let metrics = Arc::new(BuilderMetrics::default());

        // Set last_active to a timestamp far in the past (already expired)
        let expired_ts = now_secs().saturating_sub(200);
        let last_active = Arc::new(AtomicU64::new(expired_ts));
        let (close_tx, mut close_rx) = mpsc::channel(1);
        let ip: IpAddr = "192.168.1.1".parse().unwrap();

        registry.insert(1, ConnHandle { last_active: Arc::clone(&last_active), close_tx, ip });

        let timeout = Duration::from_secs(90);
        let registry_clone = Arc::clone(&registry);
        let metrics_clone = Arc::clone(&metrics);
        tokio::spawn(idle_sweep_loop(registry_clone, timeout, metrics_clone));

        // The sweeper's first real tick is after 15s, but the connection is already
        // expired, so it should send "idle timeout" quickly.
        let msg = tokio::time::timeout(Duration::from_secs(20), close_rx.recv()).await;
        assert!(msg.is_ok(), "Expected idle timeout message within 20 seconds");
        assert_eq!(msg.unwrap(), Some("idle timeout"));
    }
}
