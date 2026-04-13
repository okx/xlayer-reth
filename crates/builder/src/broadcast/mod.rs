mod behaviour;
mod outgoing;
pub(crate) mod payload;
pub mod peer_status;
pub(crate) mod types;
pub(crate) mod wspub;

use behaviour::Behaviour;
pub use libp2p::{Multiaddr, StreamProtocol};
pub use payload::{XLayerFlashblockEnd, XLayerFlashblockMessage, XLayerFlashblockPayload};
pub use peer_status::PeerStatusTracker;
pub use types::Message;
pub use wspub::WebSocketPublisher;

use crate::metrics::BuilderMetrics;
use eyre::Context;
use futures::stream::FuturesUnordered;
use libp2p::{
    dns,
    identity::{self, ed25519},
    noise,
    swarm::{Stream, SwarmEvent},
    tcp, yamux, PeerId, Swarm, Transport as _,
};
use libp2p_stream::{Control, IncomingStreams};
use multiaddr::Protocol;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const DEFAULT_MAX_PEER_COUNT: u32 = 50;
const DEFAULT_PEER_RETRY_INTERVAL: Duration = Duration::from_secs(1);

type PendingStreamFuture =
    Pin<Box<dyn Future<Output = (PeerId, StreamProtocol, eyre::Result<Stream>)> + Send>>;

/// Current pending streams with connected peers that are being established.
#[derive(Default)]
struct PendingStreams {
    futures: FuturesUnordered<PendingStreamFuture>,
    inflight_peers: HashSet<(PeerId, StreamProtocol)>,
}

impl PendingStreams {
    /// Push an `open_stream` future for the given peer and protocol.
    /// No-op if an identical `(peer_id, protocol)` is already in-flight.
    /// When the guard fires, `control` is dropped without I/O.
    fn push(&mut self, peer_id: PeerId, proto: StreamProtocol, mut control: Control) {
        let key = (peer_id, proto.clone());
        if self.inflight_peers.contains(&key) {
            return;
        }
        self.inflight_peers.insert(key);
        self.futures.push(Box::pin(async move {
            let result = control.open_stream(peer_id, proto.clone()).await.map_err(Into::into);
            (peer_id, proto, result)
        }));
    }

    async fn next(&mut self) -> Option<(PeerId, StreamProtocol, eyre::Result<Stream>)> {
        use futures::StreamExt as _;
        let result = self.futures.next().await?;
        self.inflight_peers.remove(&(result.0, result.1.clone()));
        Some(result)
    }
}

/// The broadcast node.
///
/// The current behaviour of the node regarding messaging protocols is as follows:
/// - for each supported protocol, the node will accept incoming streams from remote peers on that
///   protocol.
/// - when a new connection is established with a peer, the node will open outbound streams to that
///   peer for each supported protocol.
/// - when a new outgoing message is received on `outgoing_message_rx`, the node will broadcast that
///   message to all connected peers that have an outbound stream open for the message's protocol.
/// - incoming messages received on incoming streams are handled by `IncomingStreamsHandler`, which
///   reads messages from the stream and sends them to a channel for processing by the consumer of
///   this library.
///
/// Currently, there is no gossip implemented; messages are simply broadcast to connected peers.
pub struct Node {
    /// The peer ID of this node.
    peer_id: PeerId,

    /// The multiaddresses this node is listening on.
    listen_addrs: Vec<libp2p::Multiaddr>,

    /// The libp2p swarm, which contains the state of the network
    /// and its behaviours.
    swarm: Swarm<Behaviour>,

    /// The multiaddresses of known peers to connect to on startup.
    known_peers: Vec<Multiaddr>,

    /// Receiver for outgoing messages to be sent to peers.
    outgoing_message_rx: mpsc::Receiver<Message>,

    /// Handler for managing outgoing streams to peers.
    /// Used to determine what peers to broadcast to when a
    /// new outgoing message is received on `outgoing_message_rx`.
    outgoing_streams_handler: outgoing::StreamsHandler,

    /// Handlers for incoming streams (streams which remote peers have opened with us).
    incoming_streams_handlers: Vec<IncomingStreamsHandler>,

    /// The protocols this node supports.
    protocols: Vec<StreamProtocol>,

    /// Cancellation token to shut down the node.
    cancellation_token: CancellationToken,

    /// WebSocket publisher for broadcasting flashblocks
    /// to all connected subscribers.
    ws_pub: Arc<WebSocketPublisher>,

    /// The metrics for the builder
    metrics: Arc<BuilderMetrics>,

    /// Shared peer status tracker for RPC visibility.
    peer_status: PeerStatusTracker,
}

impl Node {
    /// Returns the multiaddresses that this node is listening on, with the peer ID included.
    pub fn multiaddrs(&self) -> Vec<libp2p::Multiaddr> {
        self.listen_addrs
            .iter()
            .map(|addr| addr.clone().with_p2p(self.peer_id).expect("can add peer ID to multiaddr"))
            .collect()
    }

    /// Runs the broadcast node.
    /// 1. Dials known peers, and starts listening for incoming connections and messages.
    /// 2. Publishes flashblocks to websocket subscribers
    ///
    /// This function will run until the cancellation token is triggered.
    /// If an error occurs, it will be logged, but the node will continue running.
    pub async fn run(self) -> eyre::Result<()> {
        use libp2p::futures::StreamExt as _;

        let Node {
            peer_id: _,
            listen_addrs,
            mut swarm,
            known_peers,
            mut outgoing_message_rx,
            mut outgoing_streams_handler,
            cancellation_token,
            incoming_streams_handlers,
            protocols,
            ws_pub,
            metrics,
            peer_status,
        } = self;

        for addr in listen_addrs {
            swarm.listen_on(addr).wrap_err("swarm failed to listen on multiaddr")?;
        }

        let mut known_peers_info = Vec::new();
        let mut retry_interval = tokio::time::interval(DEFAULT_PEER_RETRY_INTERVAL);
        retry_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        for mut address in known_peers {
            let peer_id = match address.pop() {
                Some(multiaddr::Protocol::P2p(peer_id)) => peer_id,
                _ => {
                    eyre::bail!("no peer ID for known peer");
                }
            };
            swarm.add_peer_address(peer_id, address.clone());
            swarm.dial(address.clone()).wrap_err("swarm failed to dial known peer")?;
            known_peers_info.push((peer_id, address));
        }

        peer_status.register_static_peers(&known_peers_info);

        let handles = incoming_streams_handlers
            .into_iter()
            .map(|handler| tokio::spawn(handler.run()))
            .collect::<Vec<_>>();
        let mut pending_streams = PendingStreams::default();

        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    debug!(target: "payload_builder::broadcast", "cancellation token triggered, shutting down node");
                    handles.into_iter().for_each(|h| h.abort());
                    break Ok(());
                }
                _ = retry_interval.tick() => {
                    // Check for disconnected known peers and retry connection
                    let connected_peers: HashSet<PeerId> = swarm.connected_peers().copied().collect();
                    for (peer_id, address) in &known_peers_info {
                        if !connected_peers.contains(peer_id) {
                            // TCP disconnected — redial
                            debug!(target: "payload_builder::broadcast", "retrying connection to disconnected known peer {peer_id} at {address}");
                            swarm.add_peer_address(*peer_id, address.clone());
                            if let Err(e) = swarm.dial(address.clone()) {
                                warn!(target: "payload_builder::broadcast", "failed to retry dial to known peer {peer_id} at {address}: {e:?}");
                            }
                        } else if !outgoing_streams_handler.has_peer(peer_id) {
                            // TCP connected but no application stream — retry open_stream
                            debug!(target: "payload_builder::broadcast", "retrying open_stream to connected peer {peer_id} (no application stream)");
                            for proto in &protocols {
                                pending_streams.push(*peer_id, proto.clone(), swarm.behaviour_mut().new_control());
                            }
                        }
                    }
                }
                // Handle completed open_stream futures
                Some((peer_id, protocol, result)) = pending_streams.next() => {
                    match result {
                        Ok(stream) => {
                            outgoing_streams_handler.insert_peer_and_stream(peer_id, protocol.clone(), stream);
                            peer_status.on_stream_opened(peer_id);
                            debug!(target: "payload_builder::broadcast", "opened outbound stream with peer {peer_id} on protocol {protocol}");
                        }
                        Err(e) => {
                            warn!(target: "payload_builder::broadcast", "failed to open stream with peer {peer_id} on protocol {protocol}: {e:?}");
                        }
                    }
                }
                Some(message) = outgoing_message_rx.recv() => {
                    let protocol = message.protocol();
                    debug!(target: "payload_builder::broadcast", "received message to broadcast on protocol {protocol}");

                    // NOTE on broadcast ordering and failure semantics:
                    // `broadcast_message` sends to all connected peers concurrently and
                    // awaits until every peer's TCP send completes (or fails). This is a
                    // blocking wait — WS publish below only runs after all peer sends have
                    // resolved. TCP send success means the kernel accepted the bytes into
                    // the send buffer.
                    //
                    // However this means that networking layer failures are swallowed, as
                    // only serialization errors are propagated. This means WS publish may
                    // proceed as networking failures can be silently drop.
                    //
                    // This design is intentional and the re-org risk the builder trade-off
                    // for lower latency - since failures/switches in builder are very small.
                    // The less strict (no ack of message deliveries) allow a lower latency
                    // in the gossiping of new flashblock payloads to websocket subscribers.
                    match outgoing_streams_handler.broadcast_message(message.clone()).await {
                        Ok(failed_peers) => {
                            peer_status.on_broadcast_result(&failed_peers);
                            for &peer_id in &failed_peers {
                                for proto in &protocols {
                                    pending_streams.push(peer_id, proto.clone(), swarm.behaviour_mut().new_control());
                                }
                            }
                            if !failed_peers.is_empty() {
                                continue;
                            }
                        }
                        Err(e) => {
                            warn!(target: "payload_builder::broadcast", "failed to broadcast message on protocol {protocol}: {e:?}");
                            continue;
                        }
                    }
                    if let Message::OpFlashblockPayload(ref fb_payload) = message {
                        match ws_pub.publish(fb_payload) {
                            Ok(flashblock_byte_size) => {
                                metrics.flashblock_byte_size_histogram.record(flashblock_byte_size as f64);
                            }
                            Err(e) => {
                                warn!(target: "payload_builder::broadcast", "failed to publish flashblock to ws subscribers: {e:?}");
                            }
                        }
                    }
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr {
                            address,
                            ..
                        } => {
                            debug!(target: "payload_builder::broadcast", "new listen address: {address}");
                        }
                        SwarmEvent::ExternalAddrConfirmed { address } => {
                            debug!(target: "payload_builder::broadcast", "external address confirmed: {address}");
                        }
                        SwarmEvent::ConnectionEstablished {
                            peer_id,
                            connection_id,
                            ..
                        } => {
                            // when a new connection is established, open outbound streams for each protocol
                            // and add them to the outgoing streams handler.
                            debug!(target: "payload_builder::broadcast", "fb p2p connection established with peer {peer_id} on connection {connection_id}");
                            peer_status.on_connected(peer_id, None);
                            if !outgoing_streams_handler.has_peer(&peer_id) {
                                for protocol in &protocols {
                                    pending_streams.push(peer_id, protocol.clone(), swarm.behaviour_mut().new_control());
                                }
                            }
                        }
                        SwarmEvent::ConnectionClosed {
                            peer_id,
                            cause,
                            ..
                        } => {
                            debug!(target: "payload_builder::broadcast", "connection closed with peer {peer_id}: {cause:?}");
                            outgoing_streams_handler.remove_peer(&peer_id);
                            peer_status.on_disconnected(peer_id);
                        }
                        SwarmEvent::Behaviour(event) => event.handle(&mut swarm),
                        _ => continue,
                    }
                },
            }
        }
    }
}

pub struct NodeBuildResult {
    pub node: Node,
    pub outgoing_message_tx: mpsc::Sender<Message>,
    pub incoming_message_rxs: HashMap<StreamProtocol, mpsc::Receiver<Message>>,
    /// Shared peer status tracker — clone into the RPC layer for
    /// `eth_flashblocksPeerStatus`.
    pub peer_status: PeerStatusTracker,
}

pub struct NodeBuilder {
    port: Option<u16>,
    listen_addrs: Vec<libp2p::Multiaddr>,
    keypair_hex: Option<String>,
    known_peers: Vec<Multiaddr>,
    agent_version: Option<String>,
    protocols: Vec<StreamProtocol>,
    max_peer_count: Option<u32>,
    cancellation_token: Option<CancellationToken>,
    ws_pub: Arc<WebSocketPublisher>,
    metrics: Arc<BuilderMetrics>,
}

impl NodeBuilder {
    pub fn new(ws_pub: Arc<WebSocketPublisher>, metrics: Arc<BuilderMetrics>) -> Self {
        Self {
            port: None,
            listen_addrs: Vec::new(),
            keypair_hex: None,
            known_peers: Vec::new(),
            agent_version: None,
            protocols: Vec::new(),
            max_peer_count: None,
            cancellation_token: None,
            ws_pub,
            metrics,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    #[allow(unused)]
    pub fn with_listen_addr(mut self, addr: libp2p::Multiaddr) -> Self {
        self.listen_addrs.push(addr);
        self
    }

    pub fn with_keypair_hex_string(mut self, keypair_hex: String) -> Self {
        self.keypair_hex = Some(keypair_hex);
        self
    }

    pub fn with_agent_version(mut self, agent_version: String) -> Self {
        self.agent_version = Some(agent_version);
        self
    }

    pub fn with_protocol(mut self, protocol: StreamProtocol) -> Self {
        self.protocols.push(protocol);
        self
    }

    pub fn with_cancellation_token(
        mut self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    pub fn with_max_peer_count(mut self, max_peer_count: u32) -> Self {
        self.max_peer_count = Some(max_peer_count);
        self
    }

    pub fn with_known_peers<I, T>(mut self, addresses: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<Multiaddr>,
    {
        for address in addresses {
            self.known_peers.push(address.into());
        }
        self
    }

    pub fn try_build(self) -> eyre::Result<NodeBuildResult> {
        let Self {
            port,
            listen_addrs,
            keypair_hex,
            known_peers,
            agent_version,
            protocols,
            max_peer_count,
            cancellation_token,
            ws_pub,
            metrics,
        } = self;

        // TODO: caller should be forced to provide this
        let cancellation_token = cancellation_token.unwrap_or_default();

        let Some(agent_version) = agent_version else {
            eyre::bail!("agent version must be set");
        };

        let keypair = match keypair_hex {
            Some(hex) => {
                let mut bytes = hex::decode(hex).wrap_err("failed to decode hex string")?;
                let keypair = ed25519::Keypair::try_from_bytes(&mut bytes)
                    .wrap_err("failed to create keypair from bytes")?;
                Some(keypair.into())
            }
            None => None,
        };
        let keypair = keypair.unwrap_or(identity::Keypair::generate_ed25519());
        let peer_id = keypair.public().to_peer_id();

        let transport = create_transport(&keypair).wrap_err("failed to create transport")?;
        let max_peer_count = max_peer_count.unwrap_or(DEFAULT_MAX_PEER_COUNT);
        let mut behaviour = Behaviour::new(&keypair, agent_version, max_peer_count)
            .context("failed to create behaviour")?;
        let mut control = behaviour.new_control();

        let mut incoming_streams_handlers = Vec::new();
        let mut incoming_message_rxs = HashMap::new();
        for protocol in &protocols {
            let incoming_streams = control
                .accept(protocol.clone())
                .wrap_err("failed to subscribe to incoming streams for flashblocks protocol")?;
            let (incoming_streams_handler, message_rx) = IncomingStreamsHandler::new(
                protocol.clone(),
                incoming_streams,
                cancellation_token.clone(),
            );
            incoming_streams_handlers.push(incoming_streams_handler);
            incoming_message_rxs.insert(protocol.clone(), message_rx);
        }

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_other_transport(|_| transport)?
            .with_behaviour(|_| behaviour)?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(u64::MAX)) // don't disconnect from idle peers
            })
            .build();

        // disallow providing listen addresses that have a peer ID in them,
        // as we've specified the peer ID for this node above.
        let mut listen_addrs: Vec<Multiaddr> = listen_addrs
            .into_iter()
            .filter(|addr| {
                for protocol in addr.iter() {
                    if protocol == Protocol::P2p(peer_id) {
                        return false;
                    }
                }
                true
            })
            .collect();
        if listen_addrs.is_empty() {
            let port = port.unwrap_or(0);
            let listen_addr =
                format!("/ip4/0.0.0.0/tcp/{port}").parse().expect("can parse valid multiaddr");
            listen_addrs.push(listen_addr);
        }

        let (outgoing_message_tx, outgoing_message_rx) = tokio::sync::mpsc::channel(100);

        let peer_status = PeerStatusTracker::new(peer_id);

        Ok(NodeBuildResult {
            node: Node {
                peer_id,
                swarm,
                listen_addrs,
                known_peers,
                outgoing_message_rx,
                outgoing_streams_handler: outgoing::StreamsHandler::new(),
                cancellation_token,
                incoming_streams_handlers,
                protocols,
                ws_pub,
                metrics,
                peer_status: peer_status.clone(),
            },
            outgoing_message_tx,
            incoming_message_rxs,
            peer_status,
        })
    }
}

struct IncomingStreamsHandler {
    protocol: StreamProtocol,
    incoming: IncomingStreams,
    tx: mpsc::Sender<Message>,
    cancellation_token: CancellationToken,
}

impl IncomingStreamsHandler {
    fn new(
        protocol: StreamProtocol,
        incoming: IncomingStreams,
        cancellation_token: CancellationToken,
    ) -> (Self, mpsc::Receiver<Message>) {
        const CHANNEL_SIZE: usize = 100;
        let (tx, rx) = mpsc::channel(CHANNEL_SIZE);
        (Self { protocol, incoming, tx, cancellation_token }, rx)
    }

    async fn run(self) {
        use futures::StreamExt as _;

        let Self { protocol, mut incoming, tx, cancellation_token } = self;
        let mut handle_stream_futures = FuturesUnordered::new();

        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    debug!(target: "payload_builder::broadcast", "cancellation token triggered, shutting down incoming streams handler for protocol {protocol}");
                    return;
                }
                Some((from, stream)) = incoming.next() => {
                    debug!(target: "payload_builder::broadcast", "new incoming stream on protocol {protocol} from peer {from}");
                    handle_stream_futures.push(tokio::spawn(handle_incoming_stream(from, stream, tx.clone())));
                }
                Some(res) = handle_stream_futures.next() => {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            warn!(target: "payload_builder::broadcast", "error handling incoming stream: {e:?}");
                        }
                        Err(e) => {
                            warn!(target: "payload_builder::broadcast", "task handling incoming stream panicked: {e:?}");
                        }
                    }
                }
            }
        }
    }
}

async fn handle_incoming_stream(
    peer_id: PeerId,
    stream: Stream,
    payload_tx: mpsc::Sender<Message>,
) -> eyre::Result<()> {
    use futures::StreamExt as _;
    use tokio_util::{
        codec::{FramedRead, LinesCodec},
        compat::FuturesAsyncReadCompatExt as _,
    };

    let codec = LinesCodec::new();
    let mut reader = FramedRead::new(stream.compat(), codec);

    while let Some(res) = reader.next().await {
        match res {
            Ok(str) => {
                let payload = serde_json::from_str::<Message>(&str)
                    .wrap_err("failed to decode stream message")?;
                debug!(target: "payload_builder::broadcast", "got message from peer {peer_id}: {payload:?}");
                let _ = payload_tx.send(payload).await;
            }
            Err(e) => {
                return Err(e).wrap_err(format!("failed to read from stream of peer {peer_id}"));
            }
        }
    }

    Ok(())
}

fn create_transport(
    keypair: &identity::Keypair,
) -> eyre::Result<libp2p::core::transport::Boxed<(PeerId, libp2p::core::muxing::StreamMuxerBox)>> {
    let tcp_transport = tcp::tokio::Transport::new(tcp::Config::default());
    let dns_transport =
        dns::tokio::Transport::system(tcp_transport).wrap_err("failed to create DNS transport")?;
    let transport = dns_transport
        .upgrade(libp2p::core::upgrade::Version::V1)
        .authenticate(noise::Config::new(keypair)?)
        .multiplex(yamux::Config::default())
        .timeout(Duration::from_secs(20))
        .boxed();

    Ok(transport)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::broadcast::wspub::WebSocketPublisher;
    use crate::metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics};

    const TEST_AGENT_VERSION: &str = "test/1.0.0";

    /// Binds two ephemeral ports on 127.0.0.1, guaranteeing they are distinct.
    /// Returns `(port1, port2, guard1, guard2)` — callers must hold the guards
    /// alive until the ports are handed off to avoid TOCTOU races where the OS
    /// reassigns a released port.
    fn get_two_free_ports() -> (u16, u16, std::net::TcpListener, std::net::TcpListener) {
        let listener1 =
            std::net::TcpListener::bind("127.0.0.1:0").expect("can bind to an ephemeral port");
        let port1 = listener1.local_addr().expect("can read bound address").port();

        const MAX_RETRIES: u32 = 100;
        for _ in 0..MAX_RETRIES {
            let listener2 =
                std::net::TcpListener::bind("127.0.0.1:0").expect("can bind to an ephemeral port");
            let port2 = listener2.local_addr().expect("can read bound address").port();
            if port2 != port1 {
                return (port1, port2, listener1, listener2);
            }
        }
        panic!("failed to obtain two distinct ephemeral ports after {MAX_RETRIES} retries");
    }

    fn make_test_node_builder() -> NodeBuilder {
        let task_metrics = FlashblocksTaskMetrics::new();
        let metrics = Arc::new(BuilderMetrics::default());
        let ws_pub = Arc::new(
            WebSocketPublisher::new(
                "127.0.0.1:0".parse().unwrap(),
                metrics.clone(),
                &task_metrics.websocket_publisher,
                None,
            )
            .expect("can create test WebSocketPublisher"),
        );
        NodeBuilder::new(ws_pub, metrics)
    }

    #[tokio::test]
    async fn two_nodes_can_connect_and_message() {
        let (port1, port2, _guard1, _guard2) = get_two_free_ports();

        // Drop the guards right before building so libp2p can bind the ports.
        // The window between drop and `listen_on` is minimal.
        drop(_guard1);
        drop(_guard2);

        let NodeBuildResult {
            node: node1,
            outgoing_message_tx: _,
            incoming_message_rxs: mut rx1,
            ..
        } = make_test_node_builder()
            .with_listen_addr(format!("/ip4/127.0.0.1/tcp/{port1}").parse().unwrap())
            .with_agent_version(TEST_AGENT_VERSION.to_string())
            .with_protocol(types::FLASHBLOCKS_STREAM_PROTOCOL)
            .try_build()
            .unwrap();
        let NodeBuildResult { node: node2, outgoing_message_tx: tx2, .. } =
            make_test_node_builder()
                .with_known_peers(node1.multiaddrs())
                .with_protocol(types::FLASHBLOCKS_STREAM_PROTOCOL)
                .with_listen_addr(format!("/ip4/127.0.0.1/tcp/{port2}").parse().unwrap())
                .with_agent_version(TEST_AGENT_VERSION.to_string())
                .try_build()
                .unwrap();

        tokio::spawn(async move { node1.run().await });
        tokio::spawn(async move { node2.run().await });
        let message = Message::from_flashblock_payload(
            XLayerFlashblockMessage::from_flashblock_payload(XLayerFlashblockPayload::default()),
        );
        let mut rx = rx1.remove(&types::FLASHBLOCKS_STREAM_PROTOCOL).unwrap();
        let recv_message = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                // Use try_send to avoid panicking if the channel is full
                // (e.g. connection not yet established and messages are buffered).
                let _ = tx2.try_send(message.clone());
                if let Ok(Some(received)) =
                    tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
                {
                    return received;
                }
            }
        })
        .await
        .expect("message receive timed out");
        assert_eq!(recv_message, message);
    }

    /// Creates two minimal libp2p swarms (using only `libp2p_stream::Behaviour`),
    /// connects them over TCP, and returns the peer ID of A plus `libp2p_stream::Control`
    /// handles for both A and B.
    ///
    /// Both swarms are handed off to background tokio tasks after connection is
    /// established; all further interaction uses the returned controls.
    async fn connected_stream_swarms() -> (PeerId, libp2p_stream::Control, libp2p_stream::Control) {
        use libp2p::{futures::StreamExt as _, identity, swarm::SwarmEvent};

        let make_swarm = || {
            let keypair = identity::Keypair::generate_ed25519();
            let transport = create_transport(&keypair).unwrap();
            libp2p::SwarmBuilder::with_existing_identity(keypair)
                .with_tokio()
                .with_other_transport(|_| transport)
                .unwrap()
                .with_behaviour(|_| libp2p_stream::Behaviour::new())
                .unwrap()
                .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
                .build()
        };

        let mut swarm_a = make_swarm();
        let mut swarm_b = make_swarm();
        let peer_a = *swarm_a.local_peer_id();
        let control_a = swarm_a.behaviour_mut().new_control();
        let control_b = swarm_b.behaviour_mut().new_control();

        swarm_a.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();

        let addr_a = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm_a.next().await.unwrap() {
                break address.with(Protocol::P2p(peer_a));
            }
        };

        swarm_b.dial(addr_a).unwrap();

        // Drive both swarms until connection is established on both sides.
        let mut a_conn = false;
        let mut b_conn = false;
        while !(a_conn && b_conn) {
            tokio::select! {
                event = swarm_a.next() => {
                    if let Some(SwarmEvent::ConnectionEstablished { .. }) = event {
                        a_conn = true;
                    }
                }
                event = swarm_b.next() => {
                    if let Some(SwarmEvent::ConnectionEstablished { .. }) = event {
                        b_conn = true;
                    }
                }
            }
        }

        tokio::spawn(async move {
            loop {
                swarm_a.next().await;
            }
        });
        tokio::spawn(async move {
            loop {
                swarm_b.next().await;
            }
        });

        (peer_a, control_a, control_b)
    }

    /// Verifies that when the remote end of a stream is closed, `broadcast_message`
    /// returns the affected peer in `failed_peers` and evicts it from the handler.
    #[tokio::test]
    async fn broadcast_evicts_peer_on_stream_failure() {
        use libp2p::futures::StreamExt as _;

        // A registers the protocol so B can open a stream to it.
        let (peer_a, mut control_a, mut control_b) = connected_stream_swarms().await;
        let mut incoming_a = control_a.accept(types::FLASHBLOCKS_STREAM_PROTOCOL).unwrap();

        // B opens an outbound stream to A.
        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            control_b.open_stream(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL),
        )
        .await
        .expect("open_stream timed out")
        .expect("open_stream failed");

        // A accepts the stream and immediately drops it, simulating a remote stream close.
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            if let Some((_, remote_stream)) = incoming_a.next().await {
                drop(remote_stream);
                let _ = closed_tx.send(());
            }
        });
        closed_rx.await.expect("A never accepted the stream");

        // Give the close a moment to propagate through yamux.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut handler = outgoing::StreamsHandler::new();
        handler.insert_peer_and_stream(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL, stream);
        assert!(handler.has_peer(&peer_a));

        let msg = Message::from_flashblock_payload(
            XLayerFlashblockMessage::from_flashblock_payload(XLayerFlashblockPayload::default()),
        );
        let failed = handler.broadcast_message(msg).await.expect("serialization must not fail");

        assert_eq!(failed, vec![peer_a], "peer_a must be returned as a failed peer");
        assert!(!handler.has_peer(&peer_a), "peer_a must be evicted from the handler");
    }

    /// Verifies that a successful broadcast returns an empty `failed_peers` list
    /// and that the peer remains in the handler.
    #[tokio::test]
    async fn broadcast_returns_empty_failed_peers_on_success() {
        use libp2p::futures::StreamExt as _;

        // A registers the protocol so B can open a stream to it.
        let (peer_a, mut control_a, mut control_b) = connected_stream_swarms().await;
        let mut incoming_a = control_a.accept(types::FLASHBLOCKS_STREAM_PROTOCOL).unwrap();

        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            control_b.open_stream(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL),
        )
        .await
        .expect("open_stream timed out")
        .expect("open_stream failed");

        // A accepts the stream and keeps it alive for the duration of the test.
        tokio::spawn(async move {
            if let Some((_, stream)) = incoming_a.next().await {
                tokio::time::sleep(Duration::from_secs(10)).await;
                drop(stream);
            }
        });

        let mut handler = outgoing::StreamsHandler::new();
        handler.insert_peer_and_stream(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL, stream);

        let msg = Message::from_flashblock_payload(
            XLayerFlashblockMessage::from_flashblock_payload(XLayerFlashblockPayload::default()),
        );
        let failed = handler.broadcast_message(msg).await.expect("serialization must not fail");

        assert!(failed.is_empty(), "no peers should fail on a healthy stream");
        assert!(
            handler.has_peer(&peer_a),
            "peer must remain in the handler after a successful send"
        );
    }

    /// Verifies that `PendingStreams` can open a stream to a connected peer that
    /// has no application-level stream yet (the "connected but no stream" retry path).
    /// Also verifies that the inflight dedup guard prevents duplicate futures.
    #[tokio::test]
    async fn pending_streams_opens_stream_for_connected_peer() {
        use libp2p::futures::StreamExt as _;

        let (peer_a, mut control_a, control_b) = connected_stream_swarms().await;
        let mut incoming_a = control_a.accept(types::FLASHBLOCKS_STREAM_PROTOCOL).unwrap();

        let mut pending = PendingStreams::default();

        // Push an open_stream future — simulates the retry tick detecting a
        // connected peer with no application stream.
        let inflight_key = (peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL);

        pending.push(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL, control_b);
        assert!(
            pending.inflight_peers.contains(&inflight_key),
            "peer must be tracked as inflight after push"
        );

        // Dedup: second push for same (peer, protocol) must be rejected by the inflight guard.
        // The guard fires before `control` is touched, so any cloned control works.
        let futures_len_before = pending.futures.len();
        pending.push(peer_a, types::FLASHBLOCKS_STREAM_PROTOCOL, control_a.clone());
        assert_eq!(
            pending.futures.len(),
            futures_len_before,
            "duplicate push must not grow futures"
        );
        assert!(
            pending.inflight_peers.contains(&inflight_key),
            "peer must still be inflight after rejected push"
        );

        // Accept the incoming stream on node A's side so the negotiation completes.
        let accept_handle = tokio::spawn(async move {
            incoming_a.next().await.expect("should receive an incoming stream")
        });

        // Await the pending stream result.
        let result = tokio::time::timeout(Duration::from_secs(5), pending.next())
            .await
            .expect("pending_streams.next() timed out")
            .expect("pending_streams must yield a result");

        let (result_peer, result_proto, result_stream) = result;
        assert_eq!(result_peer, peer_a);
        assert_eq!(result_proto, types::FLASHBLOCKS_STREAM_PROTOCOL);
        let stream = result_stream.expect("open_stream must succeed for a connected peer");

        // Verify inflight set is cleared after completion.
        assert!(
            !pending.inflight_peers.contains(&inflight_key),
            "peer must be removed from inflight after completion"
        );

        // Verify the stream is usable: write through it and read on the other side.
        let (_, accepted_stream) = accept_handle.await.expect("accept task panicked");

        use futures::SinkExt as _;
        use tokio_util::{
            codec::{FramedRead, FramedWrite, LinesCodec},
            compat::FuturesAsyncReadCompatExt as _,
        };

        let test_message = Message::from_flashblock_payload(
            XLayerFlashblockMessage::from_flashblock_payload(XLayerFlashblockPayload::default()),
        );
        let test_payload =
            serde_json::to_string(&test_message).expect("can serialize test message");

        let mut writer = FramedWrite::new(stream.compat(), LinesCodec::new());
        writer.send(test_payload.to_string()).await.expect("write must succeed");

        let mut reader = FramedRead::new(accepted_stream.compat(), LinesCodec::new());
        let received =
            reader.next().await.expect("must receive a line").expect("read must succeed");
        assert_eq!(received, test_payload);
    }
}
