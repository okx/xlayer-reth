use core::time::Duration;
use std::sync::mpsc::SyncSender;

use reth_payload_builder::PayloadId;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::config::FlashblocksConfig;

/// Schedules and triggers flashblock builds at predetermined times during a
/// block slot. This should be created at the start of each payload building
/// job.
pub(super) struct FlashblockScheduler {
    /// Wall clock time when this scheduler was created.
    reference_system: std::time::SystemTime,
    /// Monotonic instant when this scheduler was created.
    reference_instant: tokio::time::Instant,
    /// The target number of flashblocks the scheduler will trigger.
    target_flashblocks: u64,
    /// Absolute times at which to trigger flashblock end intervals.
    send_times: Vec<tokio::time::Instant>,
}

impl FlashblockScheduler {
    pub(super) fn new(
        config: &FlashblocksConfig,
        block_time: Duration,
        payload_timestamp: u64,
    ) -> Self {
        // Capture current time for calculating relative offsets
        let reference_system = std::time::SystemTime::now();
        let reference_instant = tokio::time::Instant::now();

        // Calculate how much time remains until the payload deadline
        let remaining_time =
            compute_remaining_time(block_time, payload_timestamp, reference_system);

        let (target_flashblocks, send_times) = if let Some(remaining_time) = remaining_time {
            // Calculate the target flashblocks based on the remaining time
            let target_flashblocks =
                remaining_time.as_millis().div_ceil(config.interval.as_millis()) as u64;

            // Compute the schedule as relative durations from now
            let intervals = compute_scheduler_intervals(
                config.interval,
                config.send_offset_ms,
                config.end_buffer_ms,
                remaining_time,
                target_flashblocks,
            );

            // Convert relative durations to absolute instants for
            // tokio::time::sleep_until
            let send_times = intervals.into_iter().map(|d| reference_instant + d).collect();
            (target_flashblocks, send_times)
        } else {
            // Case when FCU is late
            (0, vec![])
        };

        Self { reference_system, reference_instant, target_flashblocks, send_times }
    }

    /// Runs the scheduler, sending flashblock triggers at the scheduled times.
    pub(super) async fn run(
        self,
        tx: SyncSender<CancellationToken>,
        block_cancel: CancellationToken,
        mut fb_cancel: CancellationToken,
        payload_id: PayloadId,
    ) {
        if self.target_flashblocks == 0 {
            // No flashblocks to schedule, return early.
            return;
        }

        let start = tokio::time::Instant::now();
        // Send immediate signal to build first flashblock right away.
        if tx.send(fb_cancel.clone()).is_err() {
            error!(
                target: "payload_builder",
                "Did not trigger first flashblock build due to payload building error or block building being cancelled"
            );
            return;
        }

        for (i, send_time) in self.send_times.into_iter().enumerate() {
            tokio::select! {
                _ = tokio::time::sleep_until(send_time) => {
                    // Cancel current flashblock building job
                    fb_cancel.cancel();

                    // Trigger next flashblock building job
                    fb_cancel = block_cancel.child_token();

                    let elapsed = start.elapsed();
                    debug!(
                        target: "payload_builder",
                        id = %payload_id,
                        flashblock_index = i + 1,
                        scheduled_time = ?(send_time - start),
                        actual_time = ?elapsed,
                        drift = ?(elapsed - (send_time - start)),
                        "Sending flashblock trigger"
                    );

                    if tx.send(fb_cancel.clone()).is_err() {
                        // receiver channel was dropped, return. this will only
                        // happen if the `build_payload` function returns, due
                        // to payload building error or the main cancellation
                        // token being cancelled.
                        error!(
                            target: "payload_builder",
                            id = %payload_id,
                            "Failed to send flashblock trigger, receiver channel was dropped"
                        );
                        return;
                    }
                }
                _ = block_cancel.cancelled() => {
                    warn!(
                        target: "payload_builder",
                        id = %payload_id,
                        missed_count = self.target_flashblocks - i as u64,
                        target_flashblocks = self.target_flashblocks,
                        "Missing flashblocks because the payload building job was cancelled too early"
                    );
                    return
                },
            }
        }
    }

    /// Returns the total number of flashblocks that will be triggered.
    pub(super) fn target_flashblocks(&self) -> u64 {
        self.target_flashblocks
    }
}

/// Computes the remaining time until the payload deadline. Calculates remaining
/// time as `payload_timestamp - now`. The result is capped at `block_time`. If
/// the timestamp is in the past (late FCU), returns `None`.
fn compute_remaining_time(
    block_time: Duration,
    payload_timestamp: u64,
    reference_system: std::time::SystemTime,
) -> Option<Duration> {
    let target_time = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(payload_timestamp);

    target_time
        .duration_since(reference_system)
        .ok()
        .filter(|d| !d.is_zero())
        .map(|d| d.min(block_time))
        .or_else(|| {
            // If we're here then the payload timestamp is in the past. This
            // happens when the FCU is really late and it also means we're
            // expecting a getPayload call basically right away, so we don't
            // have any time to build.
            let delay_ms =
                reference_system.duration_since(target_time).map_or(0, |d| d.as_millis());
            warn!(
                target: "payload_builder",
                payload_timestamp,
                delay_ms,
                "Late FCU: payload timestamp is in the past"
            );
            None
        })
}

/// Computes the scheduler send time intervals as durations relative to the
/// start instant.
fn compute_scheduler_intervals(
    flashblock_interval: Duration,
    send_offset_ms: i64,
    end_buffer_ms: u64,
    remaining_time: Duration,
    target_flashblocks: u64,
) -> Vec<Duration> {
    let first_flashblock_timing =
        calculate_first_flashblock_timing(remaining_time, flashblock_interval);

    // Apply end buffer configuration to deadline
    let deadline = remaining_time.saturating_sub(Duration::from_millis(end_buffer_ms));

    // Calculate subsequent send times, with send offset applied
    let mut send_times = Vec::with_capacity(target_flashblocks as usize);
    let mut timing = first_flashblock_timing;
    for _ in 0..target_flashblocks {
        send_times.push(apply_offset(timing.min(deadline), send_offset_ms).min(deadline));
        timing = timing.saturating_add(flashblock_interval);
    }

    send_times
}

/// Durations cannot be negative values so we need to store the offset value as
/// an int. This is a helper function to apply the signed millisecond offset to
/// a duration.
fn apply_offset(duration: Duration, offset_ms: i64) -> Duration {
    let offset_delta = offset_ms.unsigned_abs();
    if offset_ms >= 0 {
        duration.saturating_add(Duration::from_millis(offset_delta))
    } else {
        duration.saturating_sub(Duration::from_millis(offset_delta))
    }
}

/// Calculates when the first flashblock should be triggered.
fn calculate_first_flashblock_timing(remaining_time: Duration, interval: Duration) -> Duration {
    let remaining_time_ms = remaining_time.as_millis() as u64;
    let interval_ms = interval.as_millis() as u64;

    // The math is equivalent to the modulo operation except we produce a result
    // in the range of [1, interval] instead of [0, interval - 1].
    Duration::from_millis((remaining_time_ms.saturating_sub(1)) % interval_ms + 1)
}

impl std::fmt::Debug for FlashblockScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.send_times.iter().map(|t| {
                let offset = *t - self.reference_instant;
                let wall_time = self.reference_system + offset;
                let duration = wall_time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                let total_secs = duration.as_secs();
                let micros = duration.subsec_micros();
                let secs = total_secs % 60;
                let mins = (total_secs / 60) % 60;
                let hours = (total_secs / 3600) % 24;
                format!("{hours:02}:{mins:02}:{secs:02}.{micros:06}")
            }))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_offset() {
        assert_eq!(apply_offset(Duration::from_millis(100), 50), Duration::from_millis(150));
        assert_eq!(apply_offset(Duration::from_millis(100), -30), Duration::from_millis(70));
        assert_eq!(apply_offset(Duration::from_millis(100), 0), Duration::from_millis(100));
        // Should not underflow - saturates at zero
        assert_eq!(apply_offset(Duration::from_millis(50), -100), Duration::ZERO);
    }

    #[test]
    fn test_calculate_first_flashblock_timing() {
        // remaining_time exactly divisible by interval so we get the full
        // interval
        assert_eq!(
            calculate_first_flashblock_timing(
                Duration::from_millis(400),
                Duration::from_millis(200)
            ),
            Duration::from_millis(200)
        );

        // remaining_time with partial interval
        assert_eq!(
            calculate_first_flashblock_timing(
                Duration::from_millis(350),
                Duration::from_millis(200)
            ),
            Duration::from_millis(150)
        );

        // remaining_time less than interval
        assert_eq!(
            calculate_first_flashblock_timing(
                Duration::from_millis(140),
                Duration::from_millis(200)
            ),
            Duration::from_millis(140)
        );

        // remaining_time equals interval
        assert_eq!(
            calculate_first_flashblock_timing(
                Duration::from_millis(200),
                Duration::from_millis(200)
            ),
            Duration::from_millis(200)
        );

        // 1ms remaining — edge case for saturating_sub(1) = 0
        assert_eq!(
            calculate_first_flashblock_timing(Duration::from_millis(1), Duration::from_millis(200)),
            Duration::from_millis(1)
        );

        // 0ms remaining — edge case for saturating_sub(0) = 0
        assert_eq!(
            calculate_first_flashblock_timing(Duration::from_millis(0), Duration::from_millis(200)),
            Duration::from_millis(1)
        );
    }

    fn durations_ms(ms_values: &[u64]) -> Vec<Duration> {
        ms_values.iter().map(|&ms| Duration::from_millis(ms)).collect()
    }

    struct SchedulerIntervalsTestCase {
        name: &'static str,
        interval_ms: u64,
        send_offset_ms: i64,
        end_buffer_ms: u64,
        remaining_time_ms: u64,
        expected_intervals_ms: Vec<u64>,
    }

    fn check_scheduler_intervals(test_case: SchedulerIntervalsTestCase) {
        let intervals = compute_scheduler_intervals(
            Duration::from_millis(test_case.interval_ms),
            test_case.send_offset_ms,
            test_case.end_buffer_ms,
            Duration::from_millis(test_case.remaining_time_ms),
            test_case.remaining_time_ms.div_ceil(test_case.interval_ms),
        );
        assert_eq!(
            intervals,
            durations_ms(&test_case.expected_intervals_ms),
            "Failed test case '{}': interval={}ms, offset={}ms, buffer={}ms, remaining={}ms",
            test_case.name,
            test_case.interval_ms,
            test_case.send_offset_ms,
            test_case.end_buffer_ms,
            test_case.remaining_time_ms,
        );
    }

    #[test]
    fn test_compute_scheduler_intervals() {
        let test_cases = vec![
            // Basic cases
            SchedulerIntervalsTestCase {
                name: "normal timing",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 880,

                expected_intervals_ms: vec![80, 280, 480, 680, 880],
            },
            SchedulerIntervalsTestCase {
                name: "with offset and buffer",
                interval_ms: 200,
                send_offset_ms: -20,
                end_buffer_ms: 50,
                remaining_time_ms: 800,

                expected_intervals_ms: vec![180, 380, 580, 730],
            },
            SchedulerIntervalsTestCase {
                name: "late FCU (300ms remaining)",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 300,

                expected_intervals_ms: vec![100, 300],
            },
            SchedulerIntervalsTestCase {
                name: "end buffer equals remaining time",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 200,
                remaining_time_ms: 200,

                expected_intervals_ms: vec![0],
            },
            SchedulerIntervalsTestCase {
                name: "late FCU with offset and buffer combined",
                interval_ms: 200,
                send_offset_ms: -30,
                end_buffer_ms: 50,
                remaining_time_ms: 400,

                expected_intervals_ms: vec![170, 320],
            },
            SchedulerIntervalsTestCase {
                name: "no end buffer",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![200, 400, 600, 800, 1000],
            },
            // Offset-only cases (no end buffer)
            SchedulerIntervalsTestCase {
                name: "positive offset without buffer",
                interval_ms: 200,
                send_offset_ms: 20,
                end_buffer_ms: 0,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![220, 420, 620, 820, 1000],
            },
            SchedulerIntervalsTestCase {
                name: "negative offset without buffer",
                interval_ms: 200,
                send_offset_ms: -20,
                end_buffer_ms: 0,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![180, 380, 580, 780, 980],
            },
            SchedulerIntervalsTestCase {
                name: "positive offset with buffer",
                interval_ms: 200,
                send_offset_ms: 20,
                end_buffer_ms: 100,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![220, 420, 620, 820, 900],
            },
            // Edge cases
            SchedulerIntervalsTestCase {
                name: "single flashblock (remaining < interval)",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 150,

                expected_intervals_ms: vec![150],
            },
            SchedulerIntervalsTestCase {
                name: "buffer exceeds remaining time",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 200,
                remaining_time_ms: 100,

                expected_intervals_ms: vec![0],
            },
            SchedulerIntervalsTestCase {
                name: "large negative offset saturates to zero",
                interval_ms: 200,
                send_offset_ms: -500,
                end_buffer_ms: 0,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![0, 0, 100, 300, 500],
            },
            SchedulerIntervalsTestCase {
                name: "non-standard interval (300ms)",
                interval_ms: 300,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 1000,

                expected_intervals_ms: vec![100, 400, 700, 1000],
            },
            SchedulerIntervalsTestCase {
                name: "1ms remaining time",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 1,

                expected_intervals_ms: vec![1],
            },
            SchedulerIntervalsTestCase {
                name: "0ms remaining time",
                interval_ms: 200,
                send_offset_ms: 0,
                end_buffer_ms: 0,
                remaining_time_ms: 0,

                expected_intervals_ms: vec![],
            },
        ];

        for test_case in test_cases {
            check_scheduler_intervals(test_case);
        }
    }

    struct RemainingTimeTestCase {
        name: &'static str,
        block_time_ms: u64,
        reference_ms: u64,
        payload_timestamp: u64,
        expected_remaining_ms: Option<Duration>,
    }

    fn check_remaining_time(test_case: RemainingTimeTestCase) {
        let block_time = Duration::from_millis(test_case.block_time_ms);
        let reference_system =
            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(test_case.reference_ms);

        let remaining =
            compute_remaining_time(block_time, test_case.payload_timestamp, reference_system);

        assert_eq!(
            remaining,
            test_case.expected_remaining_ms,
            "Failed test case '{}': block_time={}ms, reference={}ms, timestamp={}",
            test_case.name,
            test_case.block_time_ms,
            test_case.reference_ms,
            test_case.payload_timestamp,
        );
    }

    #[test]
    fn test_compute_remaining_time() {
        let test_cases = vec![
            RemainingTimeTestCase {
                name: "future timestamp within block time",
                block_time_ms: 2000,
                reference_ms: 1_000_000,
                payload_timestamp: 1002,
                expected_remaining_ms: Some(Duration::from_millis(2000)),
            },
            RemainingTimeTestCase {
                name: "remaining exceeds block time (capped)",
                block_time_ms: 1000,
                reference_ms: 1_000_000,
                payload_timestamp: 1005,
                expected_remaining_ms: Some(Duration::from_millis(1000)),
            },
            RemainingTimeTestCase {
                name: "late FCU (844ms past timestamp)",
                block_time_ms: 1000,
                reference_ms: 1_000_844, // 1000.844 seconds
                payload_timestamp: 1000,
                expected_remaining_ms: None,
            },
            RemainingTimeTestCase {
                name: "late FCU (1ms past timestamp)",
                block_time_ms: 1000,
                reference_ms: 1_000_001, // 1000.001 seconds
                payload_timestamp: 1000,
                expected_remaining_ms: None,
            },
            RemainingTimeTestCase {
                name: "exact match (zero remaining)",
                block_time_ms: 1000,
                reference_ms: 1_000_000, // exactly 1000 seconds
                payload_timestamp: 1000,
                expected_remaining_ms: None,
            },
            RemainingTimeTestCase {
                name: "remaining exactly equals block time",
                block_time_ms: 1000,
                reference_ms: 1_000_000,
                payload_timestamp: 1001,
                expected_remaining_ms: Some(Duration::from_millis(1000)),
            },
            RemainingTimeTestCase {
                name: "sub-second remaining",
                block_time_ms: 1000,
                reference_ms: 1_000_500, // 1000.5 seconds
                payload_timestamp: 1001,
                expected_remaining_ms: Some(Duration::from_millis(500)),
            },
        ];

        for test_case in test_cases {
            check_remaining_time(test_case);
        }
    }

    #[tokio::test]
    async fn test_target_flashblocks() {
        let now = tokio::time::Instant::now();

        // Zero target → 0
        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 0,
            send_times: vec![],
        };
        assert_eq!(scheduler.target_flashblocks(), 0);

        // Single target, no send_times (immediate only) → 1
        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 1,
            send_times: vec![],
        };
        assert_eq!(scheduler.target_flashblocks(), 1);

        // Multiple targets
        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 3,
            send_times: vec![
                now + Duration::from_millis(100),
                now + Duration::from_millis(200),
                now + Duration::from_millis(300),
            ],
        };
        assert_eq!(scheduler.target_flashblocks(), 3);
    }

    #[tokio::test]
    async fn test_scheduler_run_none_returns_immediately() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let block_cancel = CancellationToken::new();
        let fb_cancel = block_cancel.child_token();
        let payload_id = PayloadId::new([0; 8]);

        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: tokio::time::Instant::now(),
            target_flashblocks: 0,
            send_times: vec![],
        };

        // Should return immediately without sending any triggers
        scheduler.run(tx, block_cancel, fb_cancel, payload_id).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_run_sends_all_triggers() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let block_cancel = CancellationToken::new();
        let fb_cancel = block_cancel.child_token();
        let payload_id = PayloadId::new([0; 8]);
        let now = tokio::time::Instant::now();

        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 3,
            send_times: vec![
                now + Duration::from_millis(200),
                now + Duration::from_millis(400),
                now + Duration::from_millis(600),
            ],
        };

        let handle = tokio::spawn(async move {
            scheduler.run(tx, block_cancel, fb_cancel, payload_id).await;
        });

        // Advance past all send times
        tokio::time::advance(Duration::from_millis(700)).await;
        handle.await.unwrap();

        // Count: 1 immediate + 3 scheduled = 4
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 4, "Expected 1 immediate + 3 scheduled triggers");
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_run_block_cancellation() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let block_cancel = CancellationToken::new();
        let fb_cancel = block_cancel.child_token();
        let payload_id = PayloadId::new([0; 8]);
        let now = tokio::time::Instant::now();

        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 3,
            send_times: vec![
                now + Duration::from_millis(200),
                now + Duration::from_millis(400),
                now + Duration::from_millis(600),
            ],
        };

        let cancel = block_cancel.clone();
        let handle = tokio::spawn(async move {
            scheduler.run(tx, block_cancel, fb_cancel, payload_id).await;
        });

        // Advance past first scheduled trigger only
        tokio::time::advance(Duration::from_millis(250)).await;
        tokio::task::yield_now().await;

        // Cancel before remaining triggers
        cancel.cancel();
        handle.await.unwrap();

        // Should have: 1 immediate + 1 scheduled = 2
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 2, "Expected 1 immediate + 1 scheduled trigger before cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_run_dropped_receiver() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<CancellationToken>(16);
        let block_cancel = CancellationToken::new();
        let fb_cancel = block_cancel.child_token();
        let payload_id = PayloadId::new([0; 8]);

        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: tokio::time::Instant::now(),
            target_flashblocks: 1,
            send_times: vec![tokio::time::Instant::now() + Duration::from_millis(200)],
        };

        // Drop receiver before running — first send should fail
        drop(rx);

        // Should return early without hanging
        scheduler.run(tx, block_cancel, fb_cancel, payload_id).await;
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_run_cancellation_token_lifecycle() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let block_cancel = CancellationToken::new();
        let fb_cancel = block_cancel.child_token();
        let payload_id = PayloadId::new([0; 8]);
        let now = tokio::time::Instant::now();

        let scheduler = FlashblockScheduler {
            reference_system: std::time::SystemTime::now(),
            reference_instant: now,
            target_flashblocks: 2,
            send_times: vec![now + Duration::from_millis(200), now + Duration::from_millis(400)],
        };

        let handle = tokio::spawn(async move {
            scheduler.run(tx, block_cancel, fb_cancel, payload_id).await;
        });

        tokio::time::advance(Duration::from_millis(500)).await;
        handle.await.unwrap();

        // Collect all tokens
        let mut tokens = vec![];
        while let Ok(token) = rx.try_recv() {
            tokens.push(token);
        }

        // 1 immediate + 2 scheduled = 3 tokens
        assert_eq!(tokens.len(), 3);

        // Each scheduled trigger cancels the previous token:
        // - tokens[0] (immediate) cancelled by first scheduled trigger
        // - tokens[1] cancelled by second scheduled trigger
        // - tokens[2] (last) is NOT cancelled — no subsequent trigger
        assert!(tokens[0].is_cancelled(), "Immediate token should be cancelled by first trigger");
        assert!(tokens[1].is_cancelled(), "First trigger token should be cancelled by second");
        assert!(!tokens[2].is_cancelled(), "Last token should not be cancelled by scheduler");
    }
}
