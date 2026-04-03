use parking_lot::Mutex;
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::Notify;
use tracing::*;

pub const EXECUTION_TASK_QUEUE_CAPACITY: usize = 5;

#[derive(Debug, Clone, Default)]
pub struct ExecutionTaskQueue {
    queue: Arc<Mutex<BTreeSet<u64>>>,
    notify: Arc<Notify>,
}

impl ExecutionTaskQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, height: u64) {
        let mut queue = self.queue.lock();
        if !queue.contains(&height) && queue.len() >= EXECUTION_TASK_QUEUE_CAPACITY {
            let evicted = queue.pop_first();
            warn!(
                target: "flashblocks",
                ?evicted,
                new_height = height,
                "Execution task queue full, evicting lowest height"
            );
        }
        queue.insert(height);
        self.notify.notify_one();
    }

    pub async fn next(&self) -> u64 {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut queue = self.queue.lock();
                if let Some(height) = queue.pop_first() {
                    return height;
                }
            }
            notified.await;
        }
    }

    pub fn flush(&self) {
        let mut queue = self.queue.lock();
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
}
