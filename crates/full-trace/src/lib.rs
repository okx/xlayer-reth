//! XLayer full trace support
//!
//! This crate provides tracing functionality for the XLayer Engine API.
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use xlayer_full_trace::Tracer;
//! use xlayer_engine_api::XLayerEngineApiBuilder;
//!
//! // Create a tracer instance
//! let tracer = Tracer::new();
//!
//! // Build the Engine API with the tracer middleware
//! // The tracer's event handlers (on_fork_choice_updated and on_new_payload)
//! // will be automatically called BEFORE Engine API methods are executed
//! let xlayer_engine_builder = XLayerEngineApiBuilder::new(op_engine_builder)
//!     .with_middleware(|| tracer.build_engine_api_tracer());
//! ```
//!
//! # Implementing Custom Event Handlers
//!
//! To add custom tracing logic, modify the `on_fork_choice_updated` and `on_new_payload`
//! methods in the `Tracer` implementation in `tracer.rs`.
//!
//! Note: Event handlers are called BEFORE the actual Engine API execution, so they
//! do not have access to the result of the API call.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod engine_api_tracer;
mod tracer;

pub use engine_api_tracer::EngineApiTracer;
pub use tracer::{BlockInfo, Tracer};
