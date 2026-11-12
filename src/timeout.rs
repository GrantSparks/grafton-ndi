//! The one rule for SDK timeouts, and the budget that a call's waits share.
//!
//! The SDK takes every wait as a `uint32_t` count of milliseconds. A finite
//! [`Duration`] is rounded up to whole milliseconds, so that only
//! [`Duration::ZERO`] becomes a non-blocking poll, and it may not exceed
//! [`MAX_TIMEOUT`]. A call that waits more than once, for a lock and then for
//! the SDK or in a retry loop, starts one [`Deadline`] from its caller's
//! timeout and takes every wait from what is left of it.

use std::time::{Duration, Instant};

use crate::{Error, Result, MAX_TIMEOUT};

/// [`MAX_TIMEOUT`] in the SDK's milliseconds.
const MAX_MILLIS: u32 = u32::MAX - 1;
const _: () = assert!(MAX_TIMEOUT.as_millis() == MAX_MILLIS as u128);

/// The SDK's millisecond count for a finite wait, rounded up, or `None` when
/// the wait exceeds [`MAX_TIMEOUT`].
pub(crate) fn sdk_millis(wait: Duration) -> Option<u32> {
    if wait > MAX_TIMEOUT {
        return None;
    }
    // At most MAX_MILLIS once rounded up, because MAX_TIMEOUT is whole
    // milliseconds, so the conversion cannot fail.
    u32::try_from(wait.as_nanos().div_ceil(1_000_000)).ok()
}

/// [`sdk_millis`], failing with [`Error::InvalidInput`] for a wait
/// that exceeds [`MAX_TIMEOUT`].
pub(crate) fn to_ms_checked(wait: Duration) -> Result<u32> {
    sdk_millis(wait).ok_or_else(|| {
        Error::InvalidInput(format!(
            "timeout {wait:?} exceeds MAX_TIMEOUT ({MAX_MILLIS} ms, about 49.7 days)"
        ))
    })
}

/// One call's wait budget: when it ends.
///
/// It is built once from the caller's timeout, so that waiting for a lock,
/// pausing between retries and every SDK call all spend the same budget.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline {
    /// When the budget started, which only tests ask.
    #[cfg(test)]
    start: Instant,
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
        Self::starting(Instant::now(), timeout)
    }

    /// A budget of `timeout` that starts at `start`: [`Self::after`], for a
    /// clock other than [`Instant::now`].
    ///
    /// # Errors
    ///
    /// As for [`Self::after`].
    pub(crate) fn starting(start: Instant, timeout: Duration) -> Result<Self> {
        to_ms_checked(timeout)?;
        let end = start.checked_add(timeout).ok_or_else(|| {
            Error::InvalidInput(format!(
                "timeout {timeout:?} ends at an instant this platform cannot represent"
            ))
        })?;
        Ok(Self {
            #[cfg(test)]
            start,
            end,
        })
    }

    /// The same budget, ending `wait` from now if that comes first.
    #[cfg(feature = "file-io")]
    pub(crate) fn within(self, wait: Duration) -> Self {
        self.within_at(Instant::now(), wait)
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

    /// The whole budget, from its start to its end.
    #[cfg(test)]
    pub(crate) fn budget(self) -> Duration {
        self.end.duration_since(self.start)
    }

    /// The time since the budget started.
    #[cfg(test)]
    pub(crate) fn elapsed(self) -> Duration {
        self.start.elapsed()
    }

    /// The time left, zero once the budget is spent.
    pub(crate) fn remaining(self) -> Duration {
        self.remaining_at(Instant::now())
    }

    /// The time left at `now`, zero once the budget is spent.
    pub(crate) fn remaining_at(self, now: Instant) -> Duration {
        self.end.saturating_duration_since(now)
    }

    /// The time left as the SDK's millisecond timeout.
    ///
    /// It is rounded up like every other wait, so the last SDK call of a
    /// budget blocks until the budget ends instead of returning at once, and
    /// it is 0 only once the budget is spent. What is left of a budget never
    /// exceeds [`MAX_TIMEOUT`].
    pub(crate) fn remaining_ms(self) -> u32 {
        sdk_millis(self.remaining()).unwrap_or(MAX_MILLIS)
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
    fn finite_waits_round_up_and_never_reach_the_indefinite_value() {
        assert_eq!(MAX_TIMEOUT, Duration::from_millis(u64::from(u32::MAX) - 1));
        for (wait, millis) in rounding_table() {
            assert_eq!(sdk_millis(wait), millis, "{wait:?}");
            match (to_ms_checked(wait), millis) {
                (Ok(value), Some(millis)) => assert_eq!(value, millis, "{wait:?}"),
                (Err(Error::InvalidInput(reason)), None) => {
                    assert!(reason.contains("MAX_TIMEOUT"), "{reason}");
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
        assert_eq!(zero.budget(), Duration::ZERO);
        assert_eq!(zero.remaining(), Duration::ZERO);
        assert_eq!(zero.remaining_ms(), 0);

        let longest = Deadline::after(MAX_TIMEOUT).unwrap();
        assert!(!longest.is_expired());
        assert_eq!(longest.budget(), MAX_TIMEOUT);
        assert!(longest.elapsed() < Duration::from_secs(60));
        // Rounded up, the time left is still the full budget, never u32::MAX.
        assert_eq!(longest.remaining_ms(), u32::MAX - 1);

        // A sub-millisecond remainder waits a whole millisecond.
        let short = Deadline::after(Duration::from_nanos(1)).unwrap();
        assert!(short.remaining_ms() <= 1);
        let short = Deadline::after(Duration::from_millis(500)).unwrap();
        assert!((1..=500).contains(&short.remaining_ms()));
    }

    #[test]
    fn a_narrowed_deadline_keeps_its_start_and_never_extends_the_budget() {
        let deadline = Deadline::after(Duration::from_secs(60)).unwrap();
        let now = Instant::now();
        let attempt = deadline.within_at(now, Duration::from_millis(100));
        assert_eq!(attempt.start, deadline.start);
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
        assert_eq!(short.remaining_at(later), Duration::ZERO);
    }

    #[test]
    fn a_deadline_can_start_at_an_instant_of_another_clock() {
        let start = Instant::now() + Duration::from_secs(3600);
        let deadline = Deadline::starting(start, Duration::from_millis(50)).unwrap();
        assert_eq!(deadline.end(), start + Duration::from_millis(50));
        assert_eq!(deadline.budget(), Duration::from_millis(50));
        assert_eq!(deadline.remaining_at(start), Duration::from_millis(50));
        assert_eq!(
            deadline.remaining_at(start + Duration::from_millis(30)),
            Duration::from_millis(20)
        );
        for (wait, millis) in rounding_table() {
            assert_eq!(
                Deadline::starting(start, wait).is_ok(),
                millis.is_some(),
                "{wait:?}"
            );
        }
    }
}
