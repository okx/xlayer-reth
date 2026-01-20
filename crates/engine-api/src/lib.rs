//! XLayer Engine API middleware and builder
//!
//! This crate provides:
//! - Core trait definitions for Engine API middleware
//! - Builder for composing middleware with OpEngineApi

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod builder;
mod middleware;

pub use builder::XLayerEngineApiBuilder;
pub use middleware::XLayerEngineApiMiddleware;
