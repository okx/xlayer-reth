use std::{
    collections::BTreeSet,
    sync::{Arc, Condvar, Mutex},
};
use tracing::*;

pub const EXECUTION_TASK_QUEUE_CAPACITY: usize = 5;

pub type ExecutionTaskQueue = Arc<(Mutex<BTreeSet<u64>>, Condvar)>;

/// Extension trait for [`ExecutionTaskQueue`] providing a flush operation.
pub trait ExecutionTaskQueueFlush {
    /// Clears all pending execution tasks from the queue.
    ///
    /// Called when a flush is detected on the flashblocks state layer (reorg or stale
    /// pending) to drain any queued block heights that were built against now-invalidated
    /// state. The execution worker will re-enter its wait loop and pick up fresh tasks
    /// from incoming flashblocks after this call.
    fn flush(&self);
}

impl ExecutionTaskQueueFlush for ExecutionTaskQueue {
    fn flush(&self) {
        match self.0.lock() {
            Ok(mut queue) => {
                let flushed = queue.len();
                queue.clear();
                if flushed > 0 {
                    warn!(
                        target: "flashblocks",
                        flushed,
                        "Execution task queue flushed on state reset"
                    );
                }
            }
            Err(err) => {
                warn!(
                    target: "flashblocks",
                    %err,
                    "Failed to flush execution task queue: mutex poisoned"
                );
            }
        }
    }
}
