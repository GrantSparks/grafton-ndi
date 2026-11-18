//! The one rule for SDK timeouts, and the budget that a call's waits share.
//!
//! The SDK takes every wait as a `uint32_t` count of milliseconds. A finite
//! [`Duration`] is rounded up to whole milliseconds, so that only
//! [`Duration::ZERO`] becomes a non-blocking poll, and it may not exceed
//! [`MAX_TIMEOUT`]. A call that waits more than once, for a lock and then for
//! the SDK or in a retry loop, starts one [`Deadline`] from its caller's
//! timeout and takes every wait from what is left of it.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use crate::{Error, Result, MAX_TIMEOUT};

/// The longest cancellable native wait. Receivers also use this bound to
/// observe pending notifications between attempts.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

mod stop_sealed {
    pub trait Sealed {}
    impl Sealed for super::NeverStop {}
    impl Sealed for &std::sync::atomic::AtomicBool {}
}

/// Whether a wait may begin another attempt. Checked only between attempts,
/// so a delivered frame is always guarded and processed before cancellation.
pub(crate) trait Stop: stop_sealed::Sealed + Copy {
    /// Whether the caller can request cancellation while a wait is running.
    const CAN_STOP: bool;
    type Stopped;
    fn check(&self) -> std::result::Result<(), Self::Stopped>;
    fn is_stopped(&self) -> bool {
        self.check().is_err()
    }
}

/// Synchronous waits have no cancellation state or atomic loads.
#[derive(Clone, Copy)]
pub(crate) struct NeverStop;
impl Stop for NeverStop {
    const CAN_STOP: bool = false;
    type Stopped = std::convert::Infallible;
    fn check(&self) -> std::result::Result<(), Self::Stopped> {
        Ok(())
    }
}
impl Stop for &AtomicBool {
    const CAN_STOP: bool = true;
    type Stopped = Stopped;
    fn check(&self) -> std::result::Result<(), Self::Stopped> {
        if self.load(Ordering::Acquire) {
            Err(Stopped)
        } else {
            Ok(())
        }
    }
}

/// A started wait observed its caller's cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stopped;

/// Native failure and cancellation remain separate until the async adapter.
pub(crate) type StopResult<T, S> = Result<std::result::Result<T, <S as Stop>::Stopped>>;

/// [`MAX_TIMEOUT`] in the SDK's milliseconds.
pub(crate) const MAX_MILLIS: u32 = u32::MAX - 1;

/// The SDK's millisecond count for a finite wait, rounded up, or `None` when
/// the wait exceeds [`MAX_TIMEOUT`].
pub(crate) fn sdk_millis(wait: Duration) -> Option<u32> {
    Timeout::checked(wait).map(Timeout::millis)
}

/// [`sdk_millis`], failing with [`Error::InvalidInput`] for a wait
/// that exceeds [`MAX_TIMEOUT`].
pub(crate) fn to_ms_checked(wait: Duration) -> Result<u32> {
    Timeout::new(wait).map(Timeout::millis)
}

/// A finite SDK wait checked before the clock that will spend it starts.
/// Retaining the duration preserves sub-millisecond precision for deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timeout(Duration);

impl Timeout {
    pub(crate) fn new(wait: Duration) -> Result<Self> {
        Self::checked(wait).ok_or(Error::InvalidInput(crate::InputDefect::TimeoutTooLong {
            requested: wait.into(),
        }))
    }

    fn checked(wait: Duration) -> Option<Self> {
        (wait <= MAX_TIMEOUT).then_some(Self(wait))
    }

    /// The original precision, for Rust waits that do not take SDK milliseconds.
    pub(crate) fn duration(self) -> Duration {
        self.0
    }

    pub(crate) fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    /// Restrict an already checked wait without validating it again.
    pub(crate) fn min(self, step: Duration) -> Self {
        Self(self.0.min(step))
    }

    pub(crate) fn millis(self) -> u32 {
        // The checked constructor and narrowing operations bound this rounded
        // value by MAX_MILLIS, as the MAX_TIMEOUT assertion above establishes.
        self.0.as_nanos().div_ceil(1_000_000) as u32
    }

    /// Spend a checked wait without needing to represent its ending instant.
    #[cfg(any(feature = "file-io", test))]
    pub(crate) fn remaining_at(self, start: Instant, now: Instant) -> Self {
        Self(self.0.saturating_sub(now.saturating_duration_since(start)))
    }
}

/// One call's wait budget: when it ends.
///
/// It is built once from the caller's timeout, so that waiting for a lock,
/// pausing between retries and every SDK call all spend the same budget.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline {
    end: Instant,
}

impl Deadline {
    /// A budget of `timeout` that starts now.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `timeout` exceeds [`MAX_TIMEOUT`],
    /// or when the instant it ends at cannot be represented.
    pub(crate) fn after(timeout: Duration) -> Result<Self> {
        Self::starting(Instant::now(), Timeout::new(timeout)?)
    }

    /// A budget of `timeout` that starts at `start`: [`Self::after`], for a
    /// clock other than [`Instant::now`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] if the ending instant cannot be represented.
    /// The timeout's SDK range has already been checked.
    pub(crate) fn starting(start: Instant, timeout: Timeout) -> Result<Self> {
        let end = start.checked_add(timeout.0).ok_or({
            Error::InvalidInput(crate::InputDefect::TimeoutInstantOverflow {
                requested: timeout.0.into(),
            })
        })?;
        Ok(Self { end })
    }

    /// The same budget, ending `wait` after `now` if that comes first.
    pub(crate) fn within_at(mut self, now: Instant, wait: Duration) -> Self {
        self.end = now
            .checked_add(wait)
            .map_or(self.end, |end| end.min(self.end));
        self
    }

    /// The instant the budget ends.
    pub(crate) fn end(self) -> Instant {
        self.end
    }

    /// The time left, zero once the budget is spent.
    pub(crate) fn remaining(self) -> Duration {
        self.remaining_at(Instant::now()).duration()
    }

    /// The time left at `now`, zero once the budget is spent.
    pub(crate) fn remaining_at(self, now: Instant) -> Timeout {
        // Normal observations follow the budget's start. Also keep an injected
        // earlier clock bounded by the checked SDK range.
        Timeout(self.end.saturating_duration_since(now).min(MAX_TIMEOUT))
    }

    /// The time left as the SDK's millisecond timeout.
    ///
    /// It is rounded up like every other wait, so the last SDK call of a
    /// budget blocks until the budget ends instead of returning at once, and
    /// it is 0 only once the budget is spent. What is left of a budget never
    /// exceeds [`MAX_TIMEOUT`].
    pub(crate) fn remaining_ms(self) -> u32 {
        self.step_ms_at(Instant::now(), MAX_TIMEOUT)
    }

    /// One bounded step of the budget, rounded up in SDK milliseconds.
    pub(crate) fn step_ms_at(self, now: Instant, step: Duration) -> u32 {
        self.remaining_at(now).min(step).millis()
    }

    /// Whether the budget is spent.
    pub(crate) fn is_expired(self) -> bool {
        Instant::now() >= self.end
    }
}

/// Each wait and the SDK timeout it becomes, `None` where it is refused.
#[cfg(test)]
pub(crate) fn rounding_table() -> [(Duration, Option<u32>); 9] {
    const MS: Duration = Duration::from_millis(1);
    const NS: Duration = Duration::from_nanos(1);
    [
        (Duration::ZERO, Some(0)),
        (NS, Some(1)),
        (Duration::from_nanos(999_999), Some(1)),
        (MS, Some(1)),
        (MS + NS, Some(2)),
        (MAX_TIMEOUT - NS, Some(u32::MAX - 1)),
        (MAX_TIMEOUT, Some(u32::MAX - 1)),
        (MAX_TIMEOUT + NS, None),
        (Duration::MAX, None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronous_stops_are_uninhabited_and_cancellable_stops_are_values() {
        fn _never(s: <NeverStop as Stop>::Stopped) -> ! {
            match s {}
        }
        let Ok(()) = NeverStop.check();
        let cancelled = AtomicBool::new(false);
        assert_eq!((&cancelled).check(), Ok(()));
        cancelled.store(true, Ordering::Release);
        assert_eq!((&cancelled).check(), Err(Stopped));
    }

    #[test]
    fn checked_waits_keep_sub_millisecond_precision_and_the_sdk_bound() {
        for (wait, millis) in rounding_table() {
            match (Timeout::new(wait), millis) {
                (Ok(timeout), Some(millis)) => {
                    assert_eq!(timeout.millis(), millis);
                    let start = Instant::now();
                    assert_eq!(timeout.remaining_at(start, start).duration(), wait);
                    assert_eq!(
                        timeout.remaining_at(start, start + wait).duration(),
                        Duration::ZERO
                    );
                }
                (
                    Err(Error::InvalidInput(crate::InputDefect::TimeoutTooLong { requested })),
                    None,
                ) => {
                    assert_eq!(requested, wait);
                }
                (other, _) => panic!("{wait:?} checked as {other:?}"),
            }
        }
    }

    #[test]
    fn checked_remainders_keep_their_bound_when_narrowed() {
        assert!(Timeout::checked(MAX_TIMEOUT).is_some());
        assert!(Timeout::checked(MAX_TIMEOUT + Duration::from_nanos(1)).is_none());
        let start = Instant::now();
        for wait in [Duration::ZERO, Duration::from_nanos(1), MAX_TIMEOUT] {
            let timeout = Timeout::checked(wait).unwrap();
            for elapsed in [Duration::ZERO, Duration::from_nanos(1), MAX_TIMEOUT] {
                let remaining = timeout.remaining_at(start, start + elapsed);
                for step in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
                    let bounded = remaining.min(step);
                    assert!(bounded.duration() <= remaining.duration());
                    assert!(bounded.duration() <= step);
                    assert!(bounded.millis() <= MAX_MILLIS);
                }
            }
        }
        let deadline = Deadline::starting(start, Timeout::checked(MAX_TIMEOUT).unwrap()).unwrap();
        // A synthetic observation before the start also preserves the SDK bound.
        assert_eq!(
            deadline
                .remaining_at(start - Duration::from_secs(1))
                .millis(),
            MAX_MILLIS
        );
    }

    #[test]
    fn deadline_steps_round_up_without_extending_the_remaining_budget() {
        let start = Instant::now();
        let deadline =
            Deadline::starting(start, Timeout::new(Duration::from_millis(2)).unwrap()).unwrap();
        assert_eq!(deadline.step_ms_at(start, Duration::ZERO), 0);
        assert_eq!(deadline.step_ms_at(start, Duration::from_nanos(1)), 1);
        assert_eq!(deadline.step_ms_at(start, Duration::MAX), 2);
        assert_eq!(
            deadline.step_ms_at(start + Duration::from_micros(1_999), Duration::MAX),
            1
        );
        assert_eq!(deadline.step_ms_at(deadline.end(), Duration::MAX), 0);
    }

    #[test]
    fn finite_waits_round_up_and_never_reach_the_indefinite_value() {
        assert_eq!(MAX_TIMEOUT, Duration::from_millis(u64::from(u32::MAX) - 1));
        for (wait, millis) in rounding_table() {
            assert_eq!(sdk_millis(wait), millis, "{wait:?}");
            match (to_ms_checked(wait), millis) {
                (Ok(value), Some(millis)) => assert_eq!(value, millis, "{wait:?}"),
                (Err(Error::InvalidInput(reason)), None) => {
                    assert!(
                        matches!(reason, crate::InputDefect::TimeoutTooLong { .. }),
                        "{reason}"
                    );
                }
                (other, _) => panic!("{wait:?} converted to {other:?}"),
            }
        }
    }

    #[test]
    fn a_deadline_is_validated_once_and_its_remainder_is_rounded_up() {
        for (wait, millis) in rounding_table() {
            assert_eq!(Deadline::after(wait).is_ok(), millis.is_some(), "{wait:?}");
        }

        let zero = Deadline::after(Duration::ZERO).unwrap();
        assert!(zero.is_expired());
        assert_eq!(zero.remaining(), Duration::ZERO);
        assert_eq!(zero.remaining_ms(), 0);

        let start = Instant::now();
        let longest = Deadline::starting(start, Timeout::new(MAX_TIMEOUT).unwrap()).unwrap();
        assert!(!longest.is_expired());
        assert_eq!(longest.end(), start + MAX_TIMEOUT);
        // Rounded up, the time left is still the full budget, never u32::MAX.
        assert_eq!(longest.remaining_at(start).millis(), u32::MAX - 1);

        // A sub-millisecond remainder waits a whole millisecond.
        let short = Deadline::after(Duration::from_nanos(1)).unwrap();
        assert!(short.remaining_ms() <= 1);
        let short = Deadline::after(Duration::from_millis(500)).unwrap();
        assert!((1..=500).contains(&short.remaining_ms()));
    }

    #[test]
    fn a_narrowed_deadline_never_extends_the_budget() {
        let deadline = Deadline::after(Duration::from_secs(60)).unwrap();
        let now = Instant::now();
        let attempt = deadline.within_at(now, Duration::from_millis(100));
        assert_eq!(attempt.end(), now + Duration::from_millis(100));
        assert!(attempt.end() <= deadline.end());
        assert!(attempt.remaining() <= Duration::from_millis(100));

        let short = Deadline::after(Duration::from_millis(5)).unwrap();
        let now = Instant::now();
        assert_eq!(
            short.within_at(now, Duration::from_secs(60)).end(),
            short.end()
        );
        assert_eq!(short.within_at(now, Duration::MAX).end(), short.end());
        // A budget spent before `now` stays spent.
        let later = short.end() + Duration::from_secs(1);
        assert_eq!(
            short.within_at(later, Duration::from_millis(100)).end(),
            short.end()
        );
        assert_eq!(short.remaining_at(later).duration(), Duration::ZERO);
    }

    #[test]
    fn a_deadline_can_start_at_an_instant_of_another_clock() {
        let start = Instant::now() + Duration::from_secs(3600);
        let deadline =
            Deadline::starting(start, Timeout::new(Duration::from_millis(50)).unwrap()).unwrap();
        assert_eq!(deadline.end(), start + Duration::from_millis(50));
        assert_eq!(
            deadline.remaining_at(start).duration(),
            Duration::from_millis(50)
        );
        assert_eq!(
            deadline
                .remaining_at(start + Duration::from_millis(30))
                .duration(),
            Duration::from_millis(20)
        );
        for (wait, millis) in rounding_table() {
            assert_eq!(
                Timeout::new(wait)
                    .and_then(|wait| Deadline::starting(start, wait))
                    .is_ok(),
                millis.is_some(),
                "{wait:?}"
            );
        }
    }
}
