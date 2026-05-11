//! X-Layer flashblocks crate.

mod cache;
mod debug;
mod persist;
mod state;
mod subscription;
mod ws;

pub mod execution;
pub mod service;

#[cfg(test)]
mod test_utils;

pub use cache::{CachedTxInfo, FlashblockStateCache, PendingSequence};
pub use execution::{
    BuildArgs, FlashblockReceipt, FlashblockSequenceValidator, XLayerEngineValidator,
    XLayerEngineValidatorBuilder,
};
pub use service::{FlashblocksPersistCtx, FlashblocksRpcCtx, FlashblocksRpcService};
pub use subscription::FlashblocksPubSub;
pub use ws::{WsFlashBlockStream, WsFrame};

use std::sync::Arc;

pub type PendingSequenceRx<N> = tokio::sync::watch::Receiver<Option<PendingSequence<N>>>;
pub type ReceivedFlashblocksRx = tokio::sync::broadcast::Receiver<Arc<WsFrame>>;
