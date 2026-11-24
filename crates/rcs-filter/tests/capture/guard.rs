//! The capture guard's lifecycle, driven against a counting scope (no EVM needed).

use alloy_primitives::{Log, B256};
use rcs_filter::callstack::SnapshotStamp;
use rcs_filter::capture::{CaptureGuard, CaptureOutcome, CapturedObservations, RcsCaptureScope};

/// An empty, available observation set: capture ran and genuinely saw no frame.
fn test_observations() -> CapturedObservations {
    CapturedObservations::new(
        SnapshotStamp { protocol_version: 1, content_version: 0, selector_digest: B256::ZERO },
        Vec::new(),
    )
}

#[test]
fn capture_guard_disarms_on_success_single_abort_on_err_no_double_abort() {
    // Drive the guard against a lifecycle-counting scope and assert: (a) a successful `finish`
    // disarms so `Drop` does NOT abort; (b) the `Err` path aborts exactly once via `Drop` (no
    // double-abort); (c) a caught panic still aborts exactly once and leaves the scope inactive.
    #[derive(Default)]
    struct CountingScope {
        starts: u32,
        finishes: u32,
        aborts: u32,
        active: bool,
    }
    impl RcsCaptureScope for CountingScope {
        fn scope_start_capture(&mut self) {
            self.starts += 1;
            self.active = true;
        }
        fn scope_finish_capture(&mut self, _result_logs: &[Log]) -> CaptureOutcome {
            self.finishes += 1;
            self.active = false;
            CaptureOutcome::Complete { logs: Vec::new(), observations: test_observations() }
        }
        fn scope_abort_capture(&mut self) {
            self.aborts += 1;
            self.active = false;
        }
    }

    // (a) success → finish disarms → Drop is a no-op.
    let mut scope = CountingScope::default();
    {
        let guard = CaptureGuard::new(&mut scope);
        let outcome = guard.finish(&[]);
        assert!(matches!(outcome, CaptureOutcome::Complete { .. }));
    }
    assert_eq!(scope.starts, 1, "start_capture runs once on construction");
    assert_eq!(scope.finishes, 1);
    assert_eq!(scope.aborts, 0, "a disarmed guard must not abort on Drop");
    assert!(!scope.active);

    // (b) Err path → guard dropped armed → exactly one abort, no double-abort.
    let mut scope = CountingScope::default();
    {
        let _guard = CaptureGuard::new(&mut scope);
        // Mirror the `evm.transact` Err path: return without `finish`; guard drops armed.
    }
    assert_eq!(scope.starts, 1);
    assert_eq!(scope.finishes, 0);
    assert_eq!(scope.aborts, 1, "the Err path aborts exactly once via Drop");
    assert!(!scope.active);

    // (c) caught panic → the guard's Drop still aborts once, leaving the scope inactive.
    let mut scope = CountingScope::default();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // silence the expected panic in test output
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = CaptureGuard::new(&mut scope);
        panic!("simulated transact panic");
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err(), "the panic must propagate out of catch_unwind");
    assert_eq!(scope.starts, 1);
    assert_eq!(scope.aborts, 1, "a caught panic still aborts exactly once via Drop");
    assert!(!scope.active, "the inspector must be left inactive after an unwind");
}
