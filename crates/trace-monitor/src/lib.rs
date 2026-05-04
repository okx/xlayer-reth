//! Transaction tracing: log transaction/block lifecycle to a file.
//! Logging is non-blocking (bounded channel + writer thread).

pub mod tracer;
pub mod transaction;
pub mod utils;

pub use tracer::{
    TransactionTracer, flush_global_tracer, get_global_tracer, init_global_tracer,
    sync_global_tracer,
};
pub use transaction::TransactionProcessId;
pub use utils::{Hash32, format_hash_hex, from_b256};
