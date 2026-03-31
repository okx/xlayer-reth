//! X-Layer flashblocks crate.

mod cache;
mod debug;
mod execution;
mod persist;
mod state;
mod subscription;
mod ws;

pub mod service;

#[cfg(test)]
mod test_utils;

pub use cache::{CachedTxInfo, FlashblockStateCache, PendingSequence};
pub use service::{FlashblocksPersistCtx, FlashblocksRpcCtx, FlashblocksRpcService};
pub use subscription::FlashblocksPubSub;
pub use ws::WsFlashBlockStream;

use std::sync::Arc;
use xlayer_builder::broadcast::XLayerFlashblockMessage;

pub type PendingSequenceRx<N> = tokio::sync::watch::Receiver<Option<PendingSequence<N>>>;
pub type ReceivedFlashblocksRx = tokio::sync::broadcast::Receiver<Arc<XLayerFlashblockMessage>>;
