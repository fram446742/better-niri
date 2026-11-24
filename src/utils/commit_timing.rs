//! Helpers for `wp_commit_timing_v1` deadline scheduling.
//!
//! Commit-timer deadlines are signalled per output at the output's next target presentation
//! time. Deadlines that fall further in the future are signalled by a timer that wakes the event
//! loop on the refresh cycle whose target presentation time the deadline falls on, so clients
//! waiting on a commit timer make forward progress even when nothing else wakes the loop.

use std::time::Duration;

/// Fallback refresh interval for outputs that do not report one.
const FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_millis(16);

/// Returns when to arm the commit-timing wakeup timer for the earliest still-pending deadline of
/// an output.
///
/// `target_presentation_time` is the output's next target presentation time and `refresh_interval`
/// its refresh interval. `next_deadline` is the earliest pending commit-timer deadline on the
/// output, `now` the current monotonic time.
///
/// The result is aligned to the refresh cycle on whose target presentation time the deadline
/// falls. On idle/VRR outputs the target presentation time can be "now", which would make the
/// quantized wakeup already due; in that case the deadline itself is returned instead of spinning
/// on an immediately-expired timer.
pub fn next_wakeup_time(
    target_presentation_time: Duration,
    refresh_interval: Option<Duration>,
    next_deadline: Duration,
    now: Duration,
) -> Duration {
    let refresh_interval = refresh_interval.unwrap_or(FALLBACK_REFRESH_INTERVAL);
    let steps = next_deadline
        .saturating_sub(target_presentation_time)
        .div_duration_f64(refresh_interval) as u32;
    let mut next_schedule = refresh_interval
        .saturating_mul(steps)
        .saturating_add(target_presentation_time);

    if next_schedule <= now {
        next_schedule = next_deadline;
    }

    next_schedule
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: Duration = Duration::from_millis(1000);
    const REFRESH: Duration = Duration::from_millis(10);

    fn wakeup(next_deadline: Duration, now: Duration) -> Duration {
        next_wakeup_time(TARGET, Some(REFRESH), next_deadline, now)
    }

    #[test]
    fn deadline_at_target_wakes_at_target() {
        assert_eq!(wakeup(TARGET, Duration::ZERO), TARGET);
    }

    #[test]
    fn deadline_on_a_later_cycle_quantizes_to_that_cycle() {
        // Deadline 3.5 cycles after the target falls on the cycle 3 intervals later.
        let deadline = TARGET + REFRESH * 7 / 2;
        assert_eq!(wakeup(deadline, Duration::ZERO), TARGET + REFRESH * 3);
    }

    #[test]
    fn deadline_just_past_target_quantizes_to_target() {
        let deadline = TARGET + Duration::from_millis(1);
        assert_eq!(wakeup(deadline, Duration::ZERO), TARGET);
    }

    #[test]
    fn already_due_quantized_wakeup_uses_deadline() {
        // The quantized wakeup would be in the past, so wake at the deadline itself instead of
        // spinning on a zero-duration timer.
        let now = TARGET + REFRESH * 2;
        let deadline = TARGET + REFRESH * 3 / 2;
        assert_eq!(wakeup(deadline, now), deadline);
    }

    #[test]
    fn deadline_before_target_uses_target_or_deadline() {
        // A deadline before the target cannot be signalled before the target presentation time.
        let deadline = TARGET - REFRESH;
        assert_eq!(wakeup(deadline, Duration::ZERO), TARGET);
        // But once the target is already behind us, wake at the deadline so it gets signalled.
        let now = TARGET + REFRESH;
        assert_eq!(wakeup(deadline, now), deadline);
    }

    #[test]
    fn uses_fallback_interval_without_one() {
        let deadline = TARGET + FALLBACK_REFRESH_INTERVAL * 2;
        assert_eq!(
            next_wakeup_time(TARGET, None, deadline, Duration::ZERO),
            TARGET + FALLBACK_REFRESH_INTERVAL * 2
        );
    }

    #[test]
    fn large_deadline_saturates_without_panicking() {
        let deadline = Duration::MAX;
        let result = next_wakeup_time(TARGET, Some(REFRESH), deadline, Duration::ZERO);
        // The multiplication saturates instead of overflowing.
        assert!(result >= TARGET);
    }
}
