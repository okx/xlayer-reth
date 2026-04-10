//! Default OP builder wrapper with reorg protection.
//!
//! Wraps the standard `OpPayloadBuilder` with P2P flashblocks cache accumulation
//! and cached transaction replay on conductor-driven failover. This module is used
//! when `flashblocks.enabled=false`.

mod builder;
mod handler;
mod service;

pub use builder::DefaultPayloadBuilder;
pub use service::DefaultBuilderServiceBuilder;
