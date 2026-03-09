//! X-Layer flashblocks crate.

pub mod execution;
pub mod handle;
pub mod subscription;

pub use execution::FlashblockCachedReceipt;

use reth_primitives_traits::NodePrimitives;
use std::sync::Arc;

pub mod types;
pub use types::{
    FlashBlock, FlashBlockCompleteSequence, FlashBlockPendingSequence, PendingBlockState,
    PendingFlashBlock, PendingStateRegistry, SequenceExecutionOutcome,
};

mod service;
pub use service::{
    create_canonical_block_channel, CanonicalBlockNotification, FlashBlockBuildInfo,
    FlashBlockService,
};

mod cache;

pub mod validation;

#[cfg(test)]
mod test_utils;

mod ws;
pub use ws::{FlashBlockDecoder, WsConnect, WsFlashBlockStream};

/// Receiver of the most recent [`PendingFlashBlock`] built out of [`FlashBlock`]s.
pub type PendingBlockRx<N> = tokio::sync::watch::Receiver<Option<PendingFlashBlock<N>>>;

/// Receiver of the sequences of [`FlashBlock`]s built.
pub type FlashBlockCompleteSequenceRx =
    tokio::sync::broadcast::Receiver<FlashBlockCompleteSequence>;

/// Receiver of received [`FlashBlock`]s from the (websocket) subscription.
pub type FlashBlockRx = tokio::sync::broadcast::Receiver<Arc<FlashBlock>>;

/// Receiver that signals whether a [`FlashBlock`] is currently being built.
pub type InProgressFlashBlockRx = tokio::sync::watch::Receiver<Option<FlashBlockBuildInfo>>;

/// Container for all flashblocks-related listeners.
///
/// Groups together the channels for flashblock-related updates.
#[derive(Debug)]
pub struct FlashblocksListeners<N: NodePrimitives> {
    /// Receiver of the most recent executed [`PendingFlashBlock`] built out of [`FlashBlock`]s.
    pub pending_block_rx: PendingBlockRx<N>,
    /// Subscription channel of the complete sequences of [`FlashBlock`]s built.
    pub flashblocks_sequence: tokio::sync::broadcast::Sender<FlashBlockCompleteSequence>,
    /// Receiver that signals whether a [`FlashBlock`] is currently being built.
    pub in_progress_rx: InProgressFlashBlockRx,
    /// Subscription channel for received flashblocks from the (websocket) connection.
    pub received_flashblocks: tokio::sync::broadcast::Sender<Arc<FlashBlock>>,
}

impl<N: NodePrimitives> FlashblocksListeners<N> {
    /// Creates a new [`FlashblocksListeners`] with the given channels.
    pub const fn new(
        pending_block_rx: PendingBlockRx<N>,
        flashblocks_sequence: tokio::sync::broadcast::Sender<FlashBlockCompleteSequence>,
        in_progress_rx: InProgressFlashBlockRx,
        received_flashblocks: tokio::sync::broadcast::Sender<Arc<FlashBlock>>,
    ) -> Self {
        Self { pending_block_rx, flashblocks_sequence, in_progress_rx, received_flashblocks }
    }
}
