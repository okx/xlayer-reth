//! X-Layer flashblocks crate.

mod cache;
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

use op_alloy_rpc_types_engine::OpFlashblockPayload;
use std::sync::Arc;

pub type PendingSequenceRx<N> = tokio::sync::watch::Receiver<Option<PendingSequence<N>>>;
pub type ReceivedFlashblocksRx = tokio::sync::broadcast::Receiver<Arc<OpFlashblockPayload>>;
