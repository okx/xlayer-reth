use libp2p::{Multiaddr, PeerId};
use parking_lot::RwLock;
use serde::Serialize;
use std::{collections::HashMap, sync::Arc, time::Instant};

/// Shared peer status tracker.
#[derive(Clone, Debug)]
pub struct PeerStatusTracker {
    inner: Arc<RwLock<PeerStatusInner>>,
}

#[derive(Debug)]
struct PeerStatusInner {
    local_peer_id: PeerId,
    peers: HashMap<PeerId, PeerEntry>,
}

#[derive(Debug, Default)]
struct PeerEntry {
    multiaddr: Option<Multiaddr>,
    is_static: bool,
    state: PeerConnectionState,
    has_stream: bool,
    connection_count: u64,
    last_broadcast_at: Option<Instant>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum PeerConnectionState {
    Connected {
        since: Instant,
    },
    Disconnected {
        since: Instant,
    },
    #[default]
    NeverConnected,
}

/// Serializable connection state for the RPC response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    Disconnected,
    NeverConnected,
}

impl PeerStatusTracker {
    /// Creates a new tracker for the given local peer.
    pub fn new(local_peer_id: PeerId) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PeerStatusInner { local_peer_id, peers: HashMap::new() })),
        }
    }

    /// Registers static (known) peers parsed from `--flashblocks.p2p_known_peers`.
    ///
    /// Called once at the start of [`super::Node::run`] after building
    /// `known_peers_info`.
    pub fn register_static_peers(&self, peers: &[(PeerId, Multiaddr)]) {
        let mut inner = self.inner.write();
        for (peer_id, addr) in peers {
            let entry = inner.peers.entry(*peer_id).or_default();
            entry.multiaddr = Some(addr.clone());
            entry.is_static = true;
        }
    }

    /// A TCP connection was established with `peer_id`.
    pub fn on_connected(&self, peer_id: PeerId, multiaddr: Option<Multiaddr>) {
        let mut inner = self.inner.write();
        let entry = inner.peers.entry(peer_id).or_default();
        entry.state = PeerConnectionState::Connected { since: Instant::now() };
        entry.connection_count += 1;
        if let Some(addr) = multiaddr {
            entry.multiaddr = Some(addr);
        }
    }

    /// The TCP connection with `peer_id` was closed.
    ///
    /// Non-static peers are removed entirely on disconnect to prevent unbounded
    /// accumulation from transient mDNS/DHT connections.
    pub fn on_disconnected(&self, peer_id: PeerId) {
        let mut inner = self.inner.write();
        if let Some(entry) = inner.peers.get_mut(&peer_id) {
            if !entry.is_static {
                inner.peers.remove(&peer_id);
                return;
            }
            entry.state = PeerConnectionState::Disconnected { since: Instant::now() };
            entry.has_stream = false;
        }
    }

    /// An application-level stream was successfully opened with `peer_id`.
    pub fn on_stream_opened(&self, peer_id: PeerId) {
        let mut inner = self.inner.write();
        if let Some(entry) = inner.peers.get_mut(&peer_id) {
            entry.has_stream = true;
        }
    }

    /// Processes the result of a broadcast attempt. Marks failed peers as having
    /// lost their stream, and updates `last_broadcast_at` for peers that
    /// successfully received the message.
    pub fn on_broadcast_result(&self, failed_peers: &[PeerId]) {
        let now = Instant::now();
        let mut inner = self.inner.write();
        for &peer_id in failed_peers {
            if let Some(entry) = inner.peers.get_mut(&peer_id) {
                entry.has_stream = false;
            }
        }
        for entry in inner.peers.values_mut() {
            if entry.has_stream {
                entry.last_broadcast_at = Some(now);
            }
        }
    }

    /// Returns a point-in-time snapshot of all peer statuses.
    pub fn snapshot(&self) -> PeerStatusSnapshot {
        let now = Instant::now();
        let inner = self.inner.read();

        let mut peers: Vec<PeerInfo> = inner
            .peers
            .iter()
            .map(|(peer_id, entry)| {
                let (connection_state, connected_duration_secs, disconnected_duration_secs) =
                    match &entry.state {
                        PeerConnectionState::Connected { since } => (
                            ConnectionState::Connected,
                            Some(now.duration_since(*since).as_secs_f64()),
                            None,
                        ),
                        PeerConnectionState::Disconnected { since } => (
                            ConnectionState::Disconnected,
                            None,
                            Some(now.duration_since(*since).as_secs_f64()),
                        ),
                        PeerConnectionState::NeverConnected => {
                            (ConnectionState::NeverConnected, None, None)
                        }
                    };

                let last_broadcast_secs_ago =
                    entry.last_broadcast_at.map(|t| now.duration_since(t).as_secs_f64());

                PeerInfo {
                    peer_id: peer_id.to_string(),
                    multiaddr: entry.multiaddr.as_ref().map(|a| a.to_string()),
                    is_static: entry.is_static,
                    connection_state,
                    has_stream: entry.has_stream,
                    connected_duration_secs,
                    disconnected_duration_secs,
                    connection_count: entry.connection_count,
                    last_broadcast_secs_ago,
                }
            })
            .collect();

        // Sort: static first, then by peer_id for deterministic output.
        peers.sort_by(|a, b| b.is_static.cmp(&a.is_static).then_with(|| a.peer_id.cmp(&b.peer_id)));

        let connected =
            peers.iter().filter(|p| p.connection_state == ConnectionState::Connected).count();
        let disconnected =
            peers.iter().filter(|p| p.connection_state == ConnectionState::Disconnected).count();
        let never_connected =
            peers.iter().filter(|p| p.connection_state == ConnectionState::NeverConnected).count();
        let static_peers = peers.iter().filter(|p| p.is_static).count();

        PeerStatusSnapshot {
            local_peer_id: inner.local_peer_id.to_string(),
            summary: PeerSummary {
                total: peers.len(),
                connected,
                disconnected,
                never_connected,
                static_peers,
            },
            peers,
        }
    }
}

/// Top-level response for `eth_flashblocksPeerStatus`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatusSnapshot {
    /// The local libp2p peer ID of this node.
    pub local_peer_id: String,
    /// Aggregate counts.
    pub summary: PeerSummary,
    /// Per-peer details.
    pub peers: Vec<PeerInfo>,
}

/// Summary statistics.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSummary {
    pub total: usize,
    pub connected: usize,
    pub disconnected: usize,
    pub never_connected: usize,
    pub static_peers: usize,
}

/// Status of a single peer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiaddr: Option<String>,
    pub is_static: bool,
    pub connection_state: ConnectionState,
    pub has_stream: bool,
    /// Seconds since connection was established (present only when connected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_duration_secs: Option<f64>,
    /// Seconds since disconnection (present only when disconnected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disconnected_duration_secs: Option<f64>,
    /// Total number of times this peer has connected.
    pub connection_count: u64,
    /// Seconds since last successful broadcast to this peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_broadcast_secs_ago: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer_id(seed: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        let kp = libp2p::identity::Keypair::ed25519_from_bytes(bytes).unwrap();
        kp.public().to_peer_id()
    }

    fn test_multiaddr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()
    }

    #[test]
    fn static_peers_start_as_never_connected() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        let addr_a = test_multiaddr(9001);
        tracker.register_static_peers(&[(peer_a, addr_a)]);

        let snap = tracker.snapshot();
        assert_eq!(snap.summary.total, 1);
        assert_eq!(snap.summary.never_connected, 1);
        assert_eq!(snap.summary.static_peers, 1);
        assert_eq!(snap.peers[0].connection_state, ConnectionState::NeverConnected);
        assert!(snap.peers[0].is_static);
        assert_eq!(snap.peers[0].connection_count, 0);
    }

    #[test]
    fn connect_transitions_from_never_connected() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        let addr_a = test_multiaddr(9001);
        tracker.register_static_peers(&[(peer_a, addr_a.clone())]);
        tracker.on_connected(peer_a, Some(addr_a));

        let snap = tracker.snapshot();
        assert_eq!(snap.summary.connected, 1);
        assert_eq!(snap.summary.never_connected, 0);
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Connected);
        assert_eq!(snap.peers[0].connection_count, 1);
        assert!(snap.peers[0].connected_duration_secs.is_some());
    }

    #[test]
    fn disconnect_static_peer_clears_stream_and_records_duration() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        tracker.register_static_peers(&[(peer_a, test_multiaddr(9001))]);
        tracker.on_connected(peer_a, None);
        tracker.on_stream_opened(peer_a);

        tracker.on_disconnected(peer_a);
        let snap = tracker.snapshot();
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Disconnected);
        assert!(!snap.peers[0].has_stream);
        assert!(snap.peers[0].disconnected_duration_secs.is_some());
    }

    #[test]
    fn disconnect_non_static_peer_removes_entry() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        tracker.on_connected(peer_a, None);
        assert_eq!(tracker.snapshot().summary.total, 1);

        tracker.on_disconnected(peer_a);
        assert_eq!(tracker.snapshot().summary.total, 0);
    }

    #[test]
    fn reconnect_increments_count_and_clears_disconnected_duration() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        tracker.register_static_peers(&[(peer_a, test_multiaddr(9001))]);
        tracker.on_connected(peer_a, None);
        tracker.on_disconnected(peer_a);
        tracker.on_connected(peer_a, None);

        let snap = tracker.snapshot();
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Connected);
        assert_eq!(snap.peers[0].connection_count, 2);
        assert!(snap.peers[0].disconnected_duration_secs.is_none());
    }

    #[test]
    fn non_static_peer_tracked_while_connected() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_b = test_peer_id(2);
        tracker.on_connected(peer_b, Some(test_multiaddr(9002)));

        let snap = tracker.snapshot();
        assert_eq!(snap.summary.total, 1);
        assert_eq!(snap.summary.static_peers, 0);
        assert!(!snap.peers[0].is_static);
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Connected);
    }

    #[test]
    fn broadcast_failure_closes_stream_but_keeps_connection() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        tracker.on_connected(peer_a, None);
        tracker.on_stream_opened(peer_a);

        // Broadcast fails for peer_a
        tracker.on_broadcast_result(&[peer_a]);

        let snap = tracker.snapshot();
        // Connection is still up, only stream is lost.
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Connected);
        assert!(!snap.peers[0].has_stream);
        // Failed peer should not get last_broadcast_at updated.
        assert!(snap.peers[0].last_broadcast_secs_ago.is_none());
    }

    #[test]
    fn broadcast_success_only_updates_peers_with_stream() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        let peer_b = test_peer_id(2);
        tracker.on_connected(peer_a, None);
        tracker.on_stream_opened(peer_a);
        tracker.on_connected(peer_b, None);

        tracker.on_broadcast_result(&[]);
        let snap = tracker.snapshot();
        let a = snap.peers.iter().find(|p| p.peer_id == peer_a.to_string()).unwrap();
        let b = snap.peers.iter().find(|p| p.peer_id == peer_b.to_string()).unwrap();
        assert!(a.last_broadcast_secs_ago.is_some());
        assert!(b.last_broadcast_secs_ago.is_none());
    }

    #[test]
    fn snapshot_sorts_static_peers_first() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let static_peer = test_peer_id(1);
        let dynamic_peer = test_peer_id(2);

        tracker.on_connected(dynamic_peer, None);
        tracker.register_static_peers(&[(static_peer, test_multiaddr(9001))]);

        let snap = tracker.snapshot();
        assert!(snap.peers[0].is_static);
        assert!(!snap.peers[1].is_static);
    }

    #[test]
    fn unknown_peer_operations_are_noop() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);
        let unknown = test_peer_id(99);

        tracker.on_disconnected(unknown);
        tracker.on_stream_opened(unknown);
        tracker.on_broadcast_result(&[unknown]);
        assert_eq!(tracker.snapshot().summary.total, 0);
    }

    #[test]
    fn existing_peer_state_preserved_on_reregistration() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let peer_a = test_peer_id(1);
        let addr_a = test_multiaddr(9001);

        // Connect first, then late static registration — should not clobber state.
        tracker.on_connected(peer_a, Some(addr_a.clone()));
        tracker.register_static_peers(&[(peer_a, addr_a)]);
        let snap = tracker.snapshot();
        assert!(snap.peers[0].is_static);
        assert_eq!(snap.peers[0].connection_state, ConnectionState::Connected);
        assert_eq!(snap.peers[0].connection_count, 1);

        // Connect with None multiaddr — should keep existing addr.
        tracker.on_connected(peer_a, None);
        assert!(tracker.snapshot().peers[0].multiaddr.is_some());
    }

    #[test]
    fn empty_tracker_produces_valid_snapshot() {
        let local = test_peer_id(0);
        let tracker = PeerStatusTracker::new(local);

        let snap = tracker.snapshot();
        assert_eq!(snap.summary.total, 0);
        assert_eq!(snap.summary.connected, 0);
        assert!(snap.peers.is_empty());
        assert_eq!(snap.local_peer_id, local.to_string());
    }
}
