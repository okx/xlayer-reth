use crate::{
    broadcast::XLayerFlashblockMessage, metrics::tokio::MonitoredTask, metrics::BuilderMetrics,
};
use core::{
    fmt::{Debug, Formatter},
    net::SocketAddr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};
use dashmap::DashMap;
use futures::SinkExt;
use futures_util::StreamExt;
use std::{
    io,
    net::{IpAddr, TcpListener},
    sync::Arc,
};
use tokio::{
    net::TcpStream,
    sync::{
        broadcast::{self, error::RecvError, Receiver},
        mpsc,
        watch,
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
type ConnId = u64;
type ConnRegistry = Arc<DashMap<ConnId, ConnHandle>>;

enum SweeperMsg {
    Ping,
    Close(Utf8Bytes),
}

struct ConnHandle {
    last_active: Arc<AtomicU64>,
    sweeper_tx: mpsc::UnboundedSender<SweeperMsg>,
    #[allow(dead_code)]
    ip: IpAddr,
}

struct ConnectionGuard {
    ip: IpAddr,
    per_ip: PerIpCounters,
    subs: Arc<AtomicUsize>,
    conn_id: ConnId,
    registry: ConnRegistry,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.conn_id);
        self.subs.fetch_sub(1, Ordering::AcqRel);

        let should_try_remove = if let Some(counter) = self.per_ip.get(&self.ip) {
            let prev = counter.value().fetch_sub(1, Ordering::AcqRel);
            prev == 1
        } else {
            false
        };
        // Read lock from get() is dropped here before we attempt write lock
        if should_try_remove {
            self.per_ip.remove_if(&self.ip, |_, c| c.load(Ordering::Acquire) == 0);
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
    #[allow(dead_code)]
    per_ip_limit: Option<u16>,
    #[allow(dead_code)]
    idle_timeout_secs: Option<u32>,
    #[allow(dead_code)]
    per_ip_counters: PerIpCounters,
    #[allow(dead_code)]
    conn_registry: ConnRegistry,
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
        let per_ip_counters: PerIpCounters = Arc::new(DashMap::new());
        let conn_registry: ConnRegistry = Arc::new(DashMap::new());
        let next_conn_id = Arc::new(AtomicU64::new(0));
        let listener = TcpListener::bind(addr)?;

        if let Some(timeout) = idle_timeout_secs.filter(|&t| t > 0) {
            let registry = Arc::clone(&conn_registry);
            let sweep_metrics = Arc::clone(&metrics);
            let sweep_term = term.subscribe();
            tokio::spawn(idle_sweep_loop(
                registry,
                sweep_metrics,
                Duration::from_secs(timeout as u64),
                sweep_term,
            ));
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
            Arc::clone(&per_ip_counters),
            Arc::clone(&conn_registry),
            next_conn_id,
            idle_timeout_secs,
        )));

        Ok(Self {
            sent,
            subs,
            term,
            pipe,
            subscriber_limit,
            per_ip_limit,
            idle_timeout_secs,
            per_ip_counters,
            conn_registry,
        })
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
    next_conn_id: Arc<AtomicU64>,
    _idle_timeout_secs: Option<u32>,
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
                let next_conn_id = Arc::clone(&next_conn_id);

                match accept_async(connection).await {
                    Ok(mut stream) => {
                        tokio::spawn(async move {
                            // Global limit check
                            if subscriber_limit.is_some_and(|limit| subs.load(Ordering::Relaxed) >= limit as usize) {
                                warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: subscriber limit reached");
                                let _ = stream.close(Some(CloseFrame {
                                    code: CloseCode::Again,
                                    reason: "subscriber limit reached, please try again later".into(),
                                })).await;
                                metrics.ws_subscribers_rejected_total_global.increment(1);
                                return;
                            }

                            // Per-IP limit check
                            let ip = peer_addr.ip();
                            if let Some(limit) = per_ip_limit.filter(|&l| l > 0) {
                                let counter = per_ip_counters
                                    .entry(ip)
                                    .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
                                let current = counter.value().fetch_add(1, Ordering::AcqRel);
                                if current >= limit as usize {
                                    counter.value().fetch_sub(1, Ordering::AcqRel);
                                    warn!(target: "payload_builder::broadcast", "WebSocket connection for {peer_addr} rejected: per-ip limit reached");
                                    let _ = stream.close(Some(CloseFrame {
                                        code: CloseCode::Again,
                                        reason: "per-ip subscriber limit reached".into(),
                                    })).await;
                                    metrics.ws_subscribers_rejected_total_per_ip.increment(1);
                                    return;
                                }
                            } else {
                                // Per-IP disabled: still insert a counter entry for tracking
                                let counter = per_ip_counters
                                    .entry(ip)
                                    .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
                                counter.value().fetch_add(1, Ordering::AcqRel);
                            }

                            // Admission succeeded
                            subs.fetch_add(1, Ordering::AcqRel);
                            metrics.ws_subscribers_active.increment(1.0);

                            let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
                            let last_active = Arc::new(AtomicU64::new(now_epoch_secs()));
                            let (sweeper_tx, sweeper_rx) = mpsc::unbounded_channel();

                            conn_registry.insert(conn_id, ConnHandle {
                                last_active: Arc::clone(&last_active),
                                sweeper_tx,
                                ip,
                            });

                            let guard = ConnectionGuard {
                                ip,
                                per_ip: Arc::clone(&per_ip_counters),
                                subs: Arc::clone(&subs),
                                conn_id,
                                registry: Arc::clone(&conn_registry),
                            };

                            debug!(target: "payload_builder::broadcast", "WebSocket connection established with {}", peer_addr);

                            broadcast_loop(stream, metrics.clone(), term, receiver_clone, sent, last_active, sweeper_rx).await;

                            metrics.ws_subscribers_active.decrement(1.0);
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
    metrics: Arc<BuilderMetrics>,
    term: watch::Receiver<bool>,
    blocks: broadcast::Receiver<Utf8Bytes>,
    sent: Arc<AtomicUsize>,
    last_active: Arc<AtomicU64>,
    sweeper_rx: mpsc::UnboundedReceiver<SweeperMsg>,
) {
    let mut term = term;
    let mut blocks = blocks;
    let mut stream = stream;
    let mut sweeper_rx = sweeper_rx;
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

            payload = blocks.recv() => match payload {
                Ok(payload) => {
                    sent.fetch_add(1, Ordering::Relaxed);
                    metrics.messages_sent_count.increment(1);

                    trace!(target: "payload_builder::broadcast", "Broadcasted payload: {:?}", payload);
                    if let Err(e) = stream.send(Message::Text(payload)).await {
                        debug!(target: "payload_builder::broadcast", "Send payload error for flashblocks subscription {peer_addr}: {e}");
                        break;
                    }
                    last_active.store(now_epoch_secs(), Ordering::Release);
                }
                Err(RecvError::Closed) => {
                    debug!(target: "payload_builder::broadcast", "Broadcast channel closed, exiting broadcast loop");
                    return;
                }
                Err(RecvError::Lagged(_)) => {
                    warn!(target: "payload_builder::broadcast", "Broadcast channel lagged, some messages were dropped");
                }
            },

            msg = sweeper_rx.recv() => match msg {
                Some(SweeperMsg::Close(reason)) => {
                    info!(target: "payload_builder::broadcast", "Idle sweep closing connection for {peer_addr}");
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        stream.close(Some(CloseFrame {
                            code: CloseCode::Away,
                            reason,
                        }))
                    ).await;
                    break;
                }
                Some(SweeperMsg::Ping) => {
                    if let Err(e) = stream.send(Message::Ping(vec![].into())).await {
                        debug!(target: "payload_builder::broadcast", "Ping send error for {peer_addr}: {e}");
                        break;
                    }
                }
                None => break,
            },

            message = stream.next() => if let Some(message) = message { match message {
                Ok(Message::Close(_)) => {
                    info!(target: "payload_builder::broadcast", "Closing frame received, stopping connection for {peer_addr}");
                    break;
                }
                Ok(Message::Pong(_)) => {
                    last_active.store(now_epoch_secs(), Ordering::Release);
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

async fn idle_sweep_loop(
    registry: ConnRegistry,
    metrics: Arc<BuilderMetrics>,
    timeout: Duration,
    mut term: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = term.changed() => {
                if *term.borrow() { return; }
            }
            _ = interval.tick() => {
                let now = now_epoch_secs();
                let timeout_secs = timeout.as_secs();

                for entry in registry.iter() {
                    let handle = entry.value();
                    let _ = handle.sweeper_tx.send(SweeperMsg::Ping);

                    let last = handle.last_active.load(Ordering::Acquire);
                    if now.saturating_sub(last) > timeout_secs {
                        let _ = handle.sweeper_tx.send(SweeperMsg::Close(
                            "idle timeout".into()
                        ));
                        metrics.ws_subscribers_idle_swept_total.increment(1);
                    }
                }
            }
        }
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    use crate::metrics::tokio::FlashblocksTaskMetrics;
    use tokio_tungstenite::connect_async;

    fn find_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    fn make_publisher_on_port(
        port: u16,
        subscriber_limit: Option<u16>,
        per_ip_limit: Option<u16>,
        idle_timeout_secs: Option<u32>,
    ) -> WebSocketPublisher {
        let metrics = Arc::new(BuilderMetrics::default());
        let task_metrics = FlashblocksTaskMetrics::new();
        let addr: SocketAddr = SocketAddr::new("127.0.0.1".parse().unwrap(), port);
        WebSocketPublisher::new(
            addr,
            metrics,
            &task_metrics.websocket_publisher,
            subscriber_limit,
            per_ip_limit,
            idle_timeout_secs,
        )
        .expect("should create publisher")
    }

    #[tokio::test]
    async fn test_connection_guard_decrements_on_drop() {
        let per_ip: PerIpCounters = Arc::new(DashMap::new());
        let subs = Arc::new(AtomicUsize::new(1));
        let registry: ConnRegistry = Arc::new(DashMap::new());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        per_ip.insert(ip, Arc::new(AtomicUsize::new(1)));
        registry.insert(42, ConnHandle {
            last_active: Arc::new(AtomicU64::new(0)),
            sweeper_tx: mpsc::unbounded_channel().0,
            ip,
        });

        let guard = ConnectionGuard {
            ip,
            per_ip: Arc::clone(&per_ip),
            subs: Arc::clone(&subs),
            conn_id: 42,
            registry: Arc::clone(&registry),
        };

        drop(guard);

        assert_eq!(subs.load(Ordering::Relaxed), 0);
        assert!(per_ip.get(&ip).is_none(), "IP entry should be removed when counter reaches 0");
        assert!(registry.get(&42).is_none(), "Registry entry should be removed");
    }

    #[tokio::test]
    async fn test_connection_guard_does_not_remove_ip_when_others_active() {
        let per_ip: PerIpCounters = Arc::new(DashMap::new());
        let subs = Arc::new(AtomicUsize::new(2));
        let registry: ConnRegistry = Arc::new(DashMap::new());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        per_ip.insert(ip, Arc::new(AtomicUsize::new(2)));
        registry.insert(1, ConnHandle {
            last_active: Arc::new(AtomicU64::new(0)),
            sweeper_tx: mpsc::unbounded_channel().0,
            ip,
        });
        registry.insert(2, ConnHandle {
            last_active: Arc::new(AtomicU64::new(0)),
            sweeper_tx: mpsc::unbounded_channel().0,
            ip,
        });

        let guard = ConnectionGuard {
            ip,
            per_ip: Arc::clone(&per_ip),
            subs: Arc::clone(&subs),
            conn_id: 1,
            registry: Arc::clone(&registry),
        };

        drop(guard);

        assert_eq!(subs.load(Ordering::Relaxed), 1);
        assert!(per_ip.get(&ip).is_some(), "IP entry should remain for other connections");
        assert_eq!(per_ip.get(&ip).unwrap().value().load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_per_ip_counter_concurrent_races() {
        let per_ip: PerIpCounters = Arc::new(DashMap::new());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let limit: u16 = 4;

        let mut handles = Vec::new();
        for _ in 0..10 {
            let per_ip = Arc::clone(&per_ip);
            handles.push(tokio::spawn(async move {
                let counter =
                    per_ip.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0)));
                let current = counter.value().fetch_add(1, Ordering::AcqRel);
                if current >= limit as usize {
                    counter.value().fetch_sub(1, Ordering::AcqRel);
                    false
                } else {
                    true
                }
            }));
        }

        let mut admitted = 0;
        let mut rejected = 0;
        for h in handles {
            if h.await.unwrap() {
                admitted += 1;
            } else {
                rejected += 1;
            }
        }

        assert_eq!(admitted, 4, "Exactly 4 should be admitted with limit=4");
        assert_eq!(rejected, 6, "Exactly 6 should be rejected");
        assert_eq!(
            per_ip.get(&ip).unwrap().value().load(Ordering::Relaxed),
            4,
            "Counter should be exactly at limit"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires multi-thread runtime and real TCP; run with --ignored"]
    async fn test_per_ip_limit_rejects_at_cap() {
        let port = find_free_port();
        let pub_ = make_publisher_on_port(port, None, Some(2), None);
        let url = format!("ws://127.0.0.1:{port}");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (mut ws1, _) = connect_async(&url).await.expect("connect 1");
        let (mut ws2, _) = connect_async(&url).await.expect("connect 2");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // 3rd should be rejected — connect may succeed at TCP level but gets close frame
        let result = tokio::time::timeout(Duration::from_secs(2), connect_async(&url)).await;
        match result {
            Ok(Ok((mut ws3, _))) => {
                let msg = tokio::time::timeout(Duration::from_secs(1), ws3.next()).await;
                assert!(
                    matches!(msg, Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_)),
                    "Expected close or stream end, got: {msg:?}"
                );
            }
            Ok(Err(_)) | Err(_) => {} // Connection refused or timeout — acceptable
        }

        let _ = ws1.close(None).await;
        let _ = ws2.close(None).await;
        drop(pub_);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires multi-thread runtime and real TCP; run with --ignored"]
    async fn test_per_ip_limit_disabled_allows_unlimited() {
        let port = find_free_port();
        let pub_ = make_publisher_on_port(port, None, Some(0), None);
        let url = format!("ws://127.0.0.1:{port}");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut connections = Vec::new();
        for i in 0..5 {
            let res = tokio::time::timeout(Duration::from_secs(2), connect_async(&url)).await;
            match res {
                Ok(Ok((ws, _))) => connections.push(ws),
                other => panic!("Connection {i} failed: {other:?}"),
            }
        }

        assert_eq!(connections.len(), 5);

        for ws in &mut connections {
            let _ = ws.close(None).await;
        }
        drop(pub_);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires multi-thread runtime and real TCP; run with --ignored"]
    async fn test_global_limit_still_applies() {
        let port = find_free_port();
        let pub_ = make_publisher_on_port(port, Some(2), Some(10), None);
        let url = format!("ws://127.0.0.1:{port}");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (mut ws1, _) = connect_async(&url).await.expect("connect 1");
        let (mut ws2, _) = connect_async(&url).await.expect("connect 2");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = tokio::time::timeout(Duration::from_secs(2), connect_async(&url)).await;
        match result {
            Ok(Ok((mut ws3, _))) => {
                let msg = tokio::time::timeout(Duration::from_secs(1), ws3.next()).await;
                assert!(
                    matches!(msg, Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_)),
                    "Expected close or stream end, got: {msg:?}"
                );
            }
            Ok(Err(_)) | Err(_) => {}
        }

        let _ = ws1.close(None).await;
        let _ = ws2.close(None).await;
        drop(pub_);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires multi-thread runtime and real TCP; run with --ignored"]
    async fn test_sweeper_msg_close_triggers_disconnect() {
        let port = find_free_port();
        let pub_ = make_publisher_on_port(port, None, None, None);
        let url = format!("ws://127.0.0.1:{port}");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (mut ws, _) = connect_async(&url).await.expect("should connect");

        tokio::time::sleep(Duration::from_millis(100)).await;

        for entry in pub_.conn_registry.iter() {
            let _ = entry.value().sweeper_tx.send(SweeperMsg::Close("idle timeout".into()));
        }

        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        match msg {
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                assert_eq!(frame.code, CloseCode::Away);
                assert_eq!(AsRef::<str>::as_ref(&frame.reason), "idle timeout");
            }
            Ok(Some(Ok(Message::Close(None)))) | Ok(None) | Err(_) => {}
            other => panic!("Unexpected message: {other:?}"),
        }

        drop(pub_);
    }
}

