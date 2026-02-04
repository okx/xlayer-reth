use std::time::Duration;

use tracing::info;

use reth_static_file_types::StaticFileSegment;

/// Progress logging interval.
pub(crate) const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Log progress for static file segment migration.
pub(crate) fn log_progress(
    segment: StaticFileSegment,
    blocks_done: u64,
    total_blocks: u64,
    entries: u64,
    elapsed: Duration,
) {
    let pct = (blocks_done * 100).checked_div(total_blocks).unwrap_or(0);
    let rate = if elapsed.as_secs() > 0 { entries / elapsed.as_secs() } else { entries };
    let eta_secs = if blocks_done > 0 && pct < 100 {
        let remaining = total_blocks.saturating_sub(blocks_done);
        let secs_per_block = elapsed.as_secs_f64() / blocks_done as f64;
        (remaining as f64 * secs_per_block) as u64
    } else {
        0
    };

    info!(
        target: "reth::cli",
        ?segment,
        progress = %format!("{blocks_done}/{total_blocks} ({pct}%)"),
        entries,
        rate_per_sec = rate,
        eta_secs,
        "Progress"
    );
}

pub(crate) fn log_rocksdb_progress(table: &'static str, entries: u64, elapsed: Duration) {
    let rate = if elapsed.as_secs() > 0 { entries / elapsed.as_secs() } else { entries };
    info!(
        target: "reth::cli",
        table,
        entries,
        elapsed_secs = elapsed.as_secs(),
        rate_per_sec = rate,
        "Progress"
    );
}
