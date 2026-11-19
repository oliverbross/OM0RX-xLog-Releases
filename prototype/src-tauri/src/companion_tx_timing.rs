//! Receipt evidence only. Queuing a future native presentation is not a start.
//! This proves passage of the native clock, never physical RF transmission.
pub fn observed_start(predicted_start_us: u64, native_clock_us: u64) -> Option<u64> {
    (predicted_start_us > 0 && native_clock_us >= predicted_start_us).then_some(predicted_start_us)
}

/// Check immediately before device resolution and again before stream play.
/// No mutex is held over native IO; callbacks independently recheck the ticket
/// and cancellation before emitting any nonzero sample.
pub fn admit_output(
    cancelled: &std::sync::atomic::AtomicBool,
    exact: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if cancelled.load(Ordering::Acquire) {
        return Err("TX output was cancelled".into());
    }
    exact()?;
    if cancelled.load(Ordering::Acquire) {
        return Err("TX output was cancelled".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_queued_future_frame_is_not_an_observed_start() {
        assert_eq!(observed_start(1_000_000, 999_999), None);
        assert_eq!(observed_start(1_000_000, 1_000_000), Some(1_000_000));
    }
    #[test]
    fn stopping_before_presentation_cannot_invent_or_later_retime_start() {
        let stopped_clock = 999_900;
        assert_eq!(observed_start(1_000_000, stopped_clock), None);
        assert_eq!(observed_start(1_000_000, stopped_clock), None);
        assert_eq!(observed_start(1_000_000, 1_100_000), Some(1_000_000));
    }
    #[test]
    fn missing_start_is_unknown_even_with_a_current_native_clock() {
        assert_eq!(observed_start(0, 2_000_000), None);
    }
    #[test]
    fn cancelled_output_never_reaches_device_admission() {
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(admit_output(&cancelled, || panic!("No late device resolution")).is_err());
    }
    #[test]
    fn retirement_and_stop_during_stream_build_both_refuse_play() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancelled = AtomicBool::new(false);
        assert!(admit_output(&cancelled, || Err("Exact ticket retired".into())).is_err());
        assert!(admit_output(&cancelled, || {
            cancelled.store(true, Ordering::Release);
            Ok(())
        })
        .is_err());
    }
}
