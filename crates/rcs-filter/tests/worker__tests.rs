use super::*;
use std::sync::atomic::AtomicU32;

/// The supervisor restarts a worker that panics (so a transient panic is not a silent
/// permanent death), after a backoff.
#[tokio::test(start_paused = true)]
async fn supervisor_restarts_worker_after_panic() {
    let attempts = Arc::new(AtomicU32::new(0));
    let counter = attempts.clone();
    let sup = spawn_supervised("test", move || {
        let counter = counter.clone();
        async move {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first run panics");
            }
            // Later run: park so the supervisor stays on this instance.
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    });

    // Allow the first run to panic and the backoff + restart to elapse.
    tokio::time::sleep(SUPERVISOR_RESTART_BACKOFF * 2).await;
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "worker must be restarted after a panic (attempts={})",
        attempts.load(Ordering::SeqCst)
    );
    sup.abort();
}

#[test]
fn retry_schedule_is_exponential_capped_and_resets() {
    let mut backoff =
        RetryBackoff::with_seed(Duration::from_millis(200), Duration::from_secs(5), 7);
    let bases: Vec<_> = (0..7).map(|_| backoff.next_base_delay()).collect();
    assert_eq!(bases, [200, 400, 800, 1600, 3200, 5000, 5000].map(Duration::from_millis));
    backoff.reset();
    assert_eq!(backoff.next_base_delay(), Duration::from_millis(200));
}

#[test]
fn retry_jitter_stays_between_half_base_and_cap() {
    let mut backoff =
        RetryBackoff::with_seed(Duration::from_millis(200), Duration::from_secs(5), 7);
    for base in [200, 400, 800, 1600, 3200, 5000] {
        let delay = backoff.next_delay();
        assert!(delay >= Duration::from_millis(base / 2));
        assert!(delay <= Duration::from_millis(base));
    }
}
