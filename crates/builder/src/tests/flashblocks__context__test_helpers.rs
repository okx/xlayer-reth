use super::*;

pub(super) fn test_observations() -> super::super::rcs_capture::CapturedObservations {
    super::super::rcs_capture::CapturedObservations::new(
        rcs_filter::callstack::SnapshotStamp {
            protocol_version: 1,
            content_version: 0,
            selector_digest: B256::ZERO,
        },
        Vec::new(),
    )
}
