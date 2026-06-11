//! Node-side integration layer for the XLayer chain-level blacklist (XLOP-1100).
//!
//! Connects the pure cross-client core ([`xlayer_blacklist`]) to op-reth's runtime:
//! the ingress mempool validator (FR-1), the block-head snapshot read (FR-4), the revm
//! inspector (FR-2), the deposit-intercepting executor wrapper (FR-2/3), and the
//! chain-id dispatch + metrics (FR-6/7).
//!
//! Every upstream trait/type used here is grounded in read source — see each module's
//! header for the exact `file:line` citations (op-reth submodule, reth git checkout,
//! extracted revm/alloy-evm). Compiling this crate requires the full op-reth tree and is
//! verified at stage 3.1 on a build-capable host.

pub mod evm_config;
pub mod executor_builder;
pub mod inspector;
pub mod pool_builder;
pub mod runtime;
pub mod validator;
pub mod view;

pub use evm_config::XLayerBlacklistEvmConfig;
pub use executor_builder::XLayerExecutorBuilder;
pub use inspector::XLayerRevmInspector;
pub use pool_builder::XLayerBlacklistPoolBuilder;
pub use runtime::{BlacklistRuntimeCtx, SnapshotHandle};
pub use validator::{BlacklistRejected, XLayerBlacklistTxValidator};
pub use view::RethMirrorViewCaller;
