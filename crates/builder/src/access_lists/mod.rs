//! Flashblock access list (FBAL) builder for EIP-7928.
//!
//! Provides a revm `Database` + `DatabaseCommit` wrapper that intercepts all EVM reads
//! and writes to build an [EIP-7928](https://eips.ethereum.org/EIPS/eip-7928) block access
//! list, tracking state changes indexed by transaction position within the block.

mod builder;
mod db;
mod types;

pub use builder::{AccountChangesBuilder, FlashblockAccessListBuilder};
pub use db::FBALBuilderDb;
pub use types::FlashblockAccessList;
