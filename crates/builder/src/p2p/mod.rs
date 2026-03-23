mod behaviour;
mod outgoing;
pub(crate) mod types;

use behaviour::Behaviour;
use libp2p_stream::IncomingStreams;

use crate::{flashblocks::utils::wspub::WebSocketPublisher, metrics::BuilderMetrics};
use eyre::Context;
use libp2p::{
    dns,
    identity::{self, ed25519},
    noise,
    swarm::SwarmEvent,
    tcp, yamux, PeerId, Swarm, Transport as _,
};
use multiaddr::Protocol;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub use libp2p::{Multiaddr, StreamProtocol};
pub(crate) use types::Message;

const DEFAULT_MAX_PEER_COUNT: u32 = 50;
const DEFAULT_PEER_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// The libp2p node.
///
/// The current behaviour of the node regarding messaging protocols is as follows:
/// - for each supported protocol, the node will accept incoming streams from remote peers on that protocol.
/// - when a new connection is established with a peer, the node will open outbound streams to that peer for each supported protocol.
/// - when a new outgoing message is received on `outgoing_message_rx`, the node will broadcast that message to all connected peers that have an outbound stream open for the message's protocol.
/// - incoming messages received on incoming streams are handled by `IncomingStreamsHandler`, which reads messages from the stream and sends them to a channel for processing by the consumer of this library.
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
}

impl Node {
    /// Returns the multiaddresses that this node is listening on, with the peer ID included.
    pub fn multiaddrs(&self) -> Vec<libp2p::Multiaddr> {
        self.listen_addrs
            .iter()
            .map(|addr| addr.clone().with_p2p(self.peer_id).expect("can add peer ID to multiaddr"))
            .collect()
    }

    /// Runs the p2p node, dials known peers, and starts listening for incoming connections and messages.
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

        let handles = incoming_streams_handlers
            .into_iter()
            .map(|handler| tokio::spawn(handler.run()))
            .collect::<Vec<_>>();

        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    debug!(target: "flashblocks-p2p", "cancellation token triggered, shutting down node");
                    handles.into_iter().for_each(|h| h.abort());
                    break Ok(());
                }
                _ = retry_interval.tick() => {
                    // Check for disconnected known peers and retry connection
                    let connected_peers: HashSet<PeerId> = swarm.connected_peers().copied().collect();
                    for (peer_id, address) in &known_peers_info {
                        if !connected_peers.contains(peer_id) {
                            debug!(target: "flashblocks-p2p", "retrying connection to disconnected known peer {peer_id} at {address}");
                            swarm.add_peer_address(*peer_id, address.clone());
                            if let Err(e) = swarm.dial(address.clone()) {
                                warn!(target: "flashblocks-p2p", "failed to retry dial to known peer {peer_id} at {address}: {e:?}");
                            }
                        }
                    }
                }
                Some(message) = outgoing_message_rx.recv() => {
                    let protocol = message.protocol();
                    debug!(target: "flashblocks-p2p", "received message to broadcast on protocol {protocol}");
                    if let Err(e) = outgoing_streams_handler.broadcast_message(message.clone()).await {
                        warn!(target: "flashblocks-p2p", "failed to broadcast message on protocol {protocol}: {e:?}");
                    }
                    if let Message::OpFlashblockPayload(ref fb_payload) = message {
                        let flashblock_byte_size = ws_pub.publish(fb_payload)?;
                        metrics.flashblock_byte_size_histogram.record(flashblock_byte_size as f64);
                    }
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr {
                            address,
                            ..
                        } => {
                            debug!(target: "flashblocks-p2p", "new listen address: {address}");
                        }
                        SwarmEvent::ExternalAddrConfirmed { address } => {
                            debug!(target: "flashblocks-p2p", "external address confirmed: {address}");
                        }
                        SwarmEvent::ConnectionEstablished {
                            peer_id,
                            connection_id,
                            ..
                        } => {
                            // when a new connection is established, open outbound streams for each protocol
                            // and add them to the outgoing streams handler.
                            debug!(target: "flashblocks-p2p", "fb p2p connection established with peer {peer_id}");
                            if !outgoing_streams_handler.has_peer(&peer_id) {
                                for protocol in &protocols {
                                        match swarm
                                        .behaviour_mut()
                                        .new_control()
                                        .open_stream(peer_id, protocol.clone())
                                        .await
                                    {
                                        Ok(stream) => { outgoing_streams_handler.insert_peer_and_stream(peer_id, protocol.clone(), stream);
                                            debug!(target: "flashblocks-p2p", "opened outbound stream with peer {peer_id} with protocol {protocol} on connection {connection_id}");
                                        }
                                        Err(e) => {
                                            warn!(target: "flashblocks-p2p", "failed to open stream with peer {peer_id} on connection {connection_id}: {e:?}");
                                        }
                                    }
                                }
                            }
                        }
                        SwarmEvent::ConnectionClosed {
                            peer_id,
                            cause,
                            ..
                        } => {
                            debug!(target: "flashblocks-p2p", "connection closed with peer {peer_id}: {cause:?}");
                            outgoing_streams_handler.remove_peer(&peer_id);
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
            },
            outgoing_message_tx,
            incoming_message_rxs,
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
        let mut handle_stream_futures = futures::stream::FuturesUnordered::new();

        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    debug!(target: "flashblocks-p2p", "cancellation token triggered, shutting down incoming streams handler for protocol {protocol}");
                    return;
                }
                Some((from, stream)) = incoming.next() => {
                    debug!(target: "flashblocks-p2p", "new incoming stream on protocol {protocol} from peer {from}");
                    handle_stream_futures.push(tokio::spawn(handle_incoming_stream(from, stream, tx.clone())));
                }
                Some(res) = handle_stream_futures.next() => {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            warn!(target: "flashblocks-p2p", "error handling incoming stream: {e:?}");
                        }
                        Err(e) => {
                            warn!(target: "flashblocks-p2p", "task handling incoming stream panicked: {e:?}");
                        }
                    }
                }
            }
        }
    }
}

async fn handle_incoming_stream(
    peer_id: PeerId,
    stream: libp2p::Stream,
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
                debug!(target: "flashblocks-p2p", "got message from peer {peer_id}: {payload:?}");
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
    use crate::{
        flashblocks::utils::wspub::WebSocketPublisher,
        metrics::{tokio::FlashblocksTaskMetrics, BuilderMetrics},
    };
    use op_alloy_rpc_types_engine::OpFlashblockPayload;

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

        let NodeBuildResult { node: node1, outgoing_message_tx: _, incoming_message_rxs: mut rx1 } =
            make_test_node_builder()
                .with_listen_addr(format!("/ip4/127.0.0.1/tcp/{port1}").parse().unwrap())
                .with_agent_version(TEST_AGENT_VERSION.to_string())
                .with_protocol(types::FLASHBLOCKS_STREAM_PROTOCOL)
                .try_build()
                .unwrap();
        let NodeBuildResult { node: node2, outgoing_message_tx: tx2, incoming_message_rxs: _ } =
            make_test_node_builder()
                .with_known_peers(node1.multiaddrs())
                .with_protocol(types::FLASHBLOCKS_STREAM_PROTOCOL)
                .with_listen_addr(format!("/ip4/127.0.0.1/tcp/{port2}").parse().unwrap())
                .with_agent_version(TEST_AGENT_VERSION.to_string())
                .try_build()
                .unwrap();

        tokio::spawn(async move { node1.run().await });
        tokio::spawn(async move { node2.run().await });
        let message = Message::from_flashblock_payload(OpFlashblockPayload::default());
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
}
