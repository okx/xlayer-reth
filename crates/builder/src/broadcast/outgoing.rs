use alloy_primitives::Bytes;
use futures::stream::FuturesUnordered;
use libp2p::{swarm::Stream, PeerId, StreamProtocol};
use std::collections::HashMap;
use tracing::{debug, warn};

pub(crate) struct StreamsHandler {
    peers_to_stream: HashMap<PeerId, HashMap<StreamProtocol, Stream>>,
}

impl StreamsHandler {
    pub(crate) fn new() -> Self {
        Self { peers_to_stream: HashMap::new() }
    }

    pub(crate) fn has_peer(&self, peer: &PeerId) -> bool {
        self.peers_to_stream.contains_key(peer)
    }

    pub(crate) fn insert_peer_and_stream(
        &mut self,
        peer: PeerId,
        protocol: StreamProtocol,
        stream: Stream,
    ) {
        self.peers_to_stream.entry(peer).or_default().insert(protocol, stream);
    }

    pub(crate) fn remove_peer(&mut self, peer: &PeerId) {
        self.peers_to_stream.remove(peer);
    }

    pub(crate) async fn broadcast_message(
        &mut self,
        protocol: StreamProtocol,
        bytes: Bytes,
    ) -> eyre::Result<Vec<PeerId>> {
        use futures::{SinkExt as _, StreamExt as _};
        use tokio_util::{
            codec::{FramedWrite, LengthDelimitedCodec},
            compat::FuturesAsyncReadCompatExt as _,
        };

        let peers = self.peers_to_stream.keys().cloned().collect::<Vec<_>>();
        let mut futures = FuturesUnordered::new();
        for peer in peers {
            let protocol_to_stream =
                self.peers_to_stream.get_mut(&peer).expect("stream map must exist for peer");
            let Some(stream) = protocol_to_stream.remove(&protocol) else {
                warn!(target: "payload_builder::broadcast", "no stream for protocol {protocol:?} to peer {peer}");
                continue;
            };
            let stream = stream.compat();
            let bytes = bytes.clone();
            let fut = async move {
                let mut writer = FramedWrite::new(stream, LengthDelimitedCodec::new());
                match writer.send(bytes.into()).await {
                    Ok(()) => Ok((peer, writer.into_inner().into_inner())),
                    Err(e) => Err((peer, eyre::eyre!(e))),
                }
            };
            futures.push(fut);
        }

        let mut failed_peers = Vec::new();
        while let Some(result) = futures.next().await {
            match result {
                Ok((peer, stream)) => {
                    let protocol_to_stream = self
                        .peers_to_stream
                        .get_mut(&peer)
                        .expect("stream map must exist for peer");
                    protocol_to_stream.insert(protocol.clone(), stream);
                }
                Err((peer, e)) => {
                    warn!(target: "payload_builder::broadcast", "failed to send payload to peer {peer}: {e:?}");
                    self.peers_to_stream.remove(&peer);
                    failed_peers.push(peer);
                }
            }
        }

        debug!(
            target: "payload_builder::broadcast",
            "broadcasted message to {} peers",
            self.peers_to_stream.len()
        );

        Ok(failed_peers)
    }
}
