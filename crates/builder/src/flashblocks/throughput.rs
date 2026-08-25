//! Execution-throughput accounting for the Flashblocks payload builder.
//!
//! Reth already emits a `Block added to canonical chain` log, but for payloads
//! that this builder pre-executes, that log's `elapsed`/`gas_throughput` only
//! reflect the cost of inserting the already-executed block into the engine
//! tree — not the EVM execution that actually happened inside the builder.
//! This module accumulates the active execution/build time inherited by the
//! finally-selected payload so the builder can emit a separate, clearly-labelled
//! throughput log that operators can trust for execution-performance analysis.

use std::time::Duration;

/// Accumulated active execution/build cost of the flashblock batches that the
/// currently-selected candidate payload has inherited.
///
/// Only successfully-built flashblocks whose result is adopted as the best
/// payload contribute here. Scheduler/channel waits, websocket/p2p propagation,
/// and downstream engine-tree insertion are deliberately excluded so the value
/// measures builder EVM execution rather than block-production rate.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ExecutionThroughput {
    /// Sum of the adopted flashblock batches' active execution/build durations.
    processing_elapsed: Duration,
    /// Number of flashblock batches composing the current candidate payload.
    flashblocks: u64,
}

impl ExecutionThroughput {
    /// Adds one adopted flashblock batch's active execution/build time.
    ///
    /// Call exactly when the batch's result becomes the best payload, so the
    /// running total always matches the work the current candidate inherited.
    pub(super) fn record_flashblock(&mut self, active: Duration) {
        self.processing_elapsed = self.processing_elapsed.saturating_add(active);
        self.flashblocks = self.flashblocks.saturating_add(1);
    }

    /// Returns a copy with the resolve-stage state-root computation folded in.
    ///
    /// Applied at most once, and only when the final state root is computed
    /// during payload resolution rather than inline in a flashblock build; when
    /// the state root was already produced during a build, its cost is already
    /// part of that batch's recorded time and no tail is added.
    pub(super) fn with_finalization_tail(mut self, tail: Duration) -> Self {
        self.processing_elapsed = self.processing_elapsed.saturating_add(tail);
        self
    }

    pub(super) fn processing_elapsed(&self) -> Duration {
        self.processing_elapsed
    }

    pub(super) fn flashblocks(&self) -> u64 {
        self.flashblocks
    }
}

/// Formats `gas_used / elapsed` as `<value><scale>gas/second`, reusing the scale
/// suffixes reth prints so the two logs line up visually for operators.
///
/// Returns `None` when the rate is undefined — no gas executed, or no measured
/// time — so callers never surface `NaN`, infinity, or a fabricated rate.
pub(super) fn format_gas_throughput(gas_used: u64, elapsed: Duration) -> Option<String> {
    let seconds = elapsed.as_secs_f64();
    if gas_used == 0 || seconds <= 0.0 {
        return None;
    }
    let rate = gas_used as f64 / seconds;
    let (value, scale) = if rate >= 1e9 {
        (rate / 1e9, "Ggas")
    } else if rate >= 1e6 {
        (rate / 1e6, "Mgas")
    } else if rate >= 1e3 {
        (rate / 1e3, "Kgas")
    } else {
        (rate, "gas")
    };
    Some(format!("{value:.2}{scale}/second"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_each_inherited_flashblock() {
        let mut t = ExecutionThroughput::default();
        assert_eq!(t.flashblocks(), 0);
        assert_eq!(t.processing_elapsed(), Duration::ZERO);

        t.record_flashblock(Duration::from_millis(10));
        t.record_flashblock(Duration::from_millis(25));
        t.record_flashblock(Duration::from_millis(5));

        // Every inherited batch is summed, not just the last one.
        assert_eq!(t.flashblocks(), 3);
        assert_eq!(t.processing_elapsed(), Duration::from_millis(40));
    }

    #[test]
    fn finalization_tail_is_added_once_and_leaves_count_unchanged() {
        let mut t = ExecutionThroughput::default();
        t.record_flashblock(Duration::from_millis(30));
        let with_tail = t.with_finalization_tail(Duration::from_millis(12));

        assert_eq!(with_tail.processing_elapsed(), Duration::from_millis(42));
        // The resolve tail is not a flashblock batch.
        assert_eq!(with_tail.flashblocks(), 1);
        // Original accumulator is untouched (tail applied to the returned copy only).
        assert_eq!(t.processing_elapsed(), Duration::from_millis(30));
    }

    #[test]
    fn throughput_matches_gas_over_elapsed_with_expected_scale() {
        // 48_850 gas over 100µs -> 488.5 Mgas/second.
        let s = format_gas_throughput(48_850, Duration::from_micros(100)).unwrap();
        assert_eq!(s, "488.50Mgas/second");

        // 2_000_000 gas over 1s -> 2.00 Mgas/second.
        assert_eq!(
            format_gas_throughput(2_000_000, Duration::from_secs(1)).unwrap(),
            "2.00Mgas/second"
        );

        // Sub-thousand rate keeps the bare gas unit.
        assert_eq!(format_gas_throughput(500, Duration::from_secs(1)).unwrap(), "500.00gas/second");

        // Giga scale.
        assert_eq!(
            format_gas_throughput(3_000_000_000, Duration::from_secs(1)).unwrap(),
            "3.00Ggas/second"
        );
    }

    #[test]
    fn zero_boundaries_never_fabricate_a_rate() {
        // Zero gas -> undefined, no fabricated throughput.
        assert!(format_gas_throughput(0, Duration::from_secs(1)).is_none());
        // Zero elapsed -> undefined rather than infinity.
        assert!(format_gas_throughput(1_000, Duration::ZERO).is_none());
        // Both zero -> still undefined.
        assert!(format_gas_throughput(0, Duration::ZERO).is_none());

        // The formatted value is always finite and free of NaN/inf markers.
        let s = format_gas_throughput(21_000, Duration::from_nanos(1)).unwrap();
        assert!(!s.contains("NaN") && !s.contains("inf"));
    }
}
