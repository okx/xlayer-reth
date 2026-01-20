//! XLayer full trace support
//!
//! This crate provides tracing functionality for the XLayer Engine API and RPC calls.
//!
//! # Features
//!
//! - **Engine API Tracing**: Trace Engine API calls like `fork_choice_updated` and `new_payload`
//! - **RPC Transaction Tracing**: Trace transaction submissions via `eth_sendRawTransaction` and `eth_sendTransaction`
//!
//! # Example Usage
//!
//! ## Engine API Tracing
//!
//! ```rust,ignore
//! use xlayer_full_trace::Tracer;
//! use xlayer_engine_api::XLayerEngineApiBuilder;
//! use std::sync::Arc;
//!
//! // Create a tracer instance
//! let tracer = Arc::new(Tracer::new(xlayer_args));
//!
//! // Clone for Engine API middleware
//! let tracer_for_engine = tracer.clone();
//!
//! // Build the Engine API with the tracer middleware
//! // The tracer's event handlers (on_fork_choice_updated and on_new_payload)
//! // will be automatically called BEFORE Engine API methods are executed
//! let xlayer_engine_builder = XLayerEngineApiBuilder::new(op_engine_builder)
//!     .with_middleware(move || tracer_for_engine.clone().build_engine_api_tracer());
//! ```
//!
//! ## RPC Transaction Tracing
//!
//! ```rust,ignore
//! use xlayer_full_trace::RpcTracerLayer;
//!
//! // Create RPC tracer layer for tracing transaction submissions
//! let rpc_tracer_layer = RpcTracerLayer::new(tracer);
//!
//! // Register as RPC middleware
//! let add_ons = op_node
//!     .add_ons()
//!     .with_rpc_middleware(rpc_tracer_layer)
//!     .with_engine_api(xlayer_engine_builder);
//! ```
//!
//! # Implementing Custom Event Handlers
//!
//! To add custom tracing logic, modify the event handler methods in the `Tracer` implementation:
//! - `on_fork_choice_updated`: Called before fork choice updates
//! - `on_new_payload`: Called before new payload execution
//! - `on_recv_transaction`: Called when a transaction is received via RPC
//!
//! Note: Event handlers are called BEFORE the actual execution, so they
//! do not have access to the result of the call.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod blockchain_tracer;
mod engine_api_tracer;
mod rpc_tracer;
mod tracer;

pub use blockchain_tracer::handle_canonical_state_stream;
pub use engine_api_tracer::EngineApiTracer;
pub use rpc_tracer::{RpcTracerLayer, RpcTracerService};
pub use tracer::{BlockInfo, Tracer};
