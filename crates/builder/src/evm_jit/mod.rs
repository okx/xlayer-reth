//! LLVM JIT-compiled EVM execution for X-Layer (OP Stack L2) — community architecture.
//!
//! Integrates [revmc](https://github.com/paradigmxyz/revmc)'s `JitBackend` + `JitEvm<EVM>`
//! wrapper. The wrapper overrides `EvmTr::frame_run` so every interpreter frame consults the
//! backend for a compiled function before falling back to the interpreter; compilation is
//! triggered fire-and-forget after a hotness threshold and runs on a dedicated worker pool.
//!
//! # Architecture
//!
//! ```text
//! JitEvmFactory ──creates──► JitOpEvm{ inner: JitEvm<op_revm::OpEvm> }
//!     │                           │ inspect=false: OpHandler::run(&mut self.inner)
//!     └── JitBackend (Arc clone)  │     └── evm.frame_run() → JitEvm override → JIT or interpret
//!                                 │ inspect=true:  OpHandler::inspect_run(&mut self.inner)
//!                                 │     └── inspect_frame_run also routes through JIT
//!                                 └── JitBackend
//! ```
//!
//! Enable with `--features jit`.

pub mod aot_store;
pub mod executor;
pub mod factory;

pub use aot_store::PersistentArtifactStore;
pub use executor::JitExecutorBuilder;
pub use factory::{JitEvmFactory, JitOpEvm};
