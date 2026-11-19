//! Bounded event draining and finalization for synchronous and async waits.

use std::time::{Duration, Instant};

use super::{
    FileRecorder, FinishStatus, RecorderEventSink, RecordingReport, SessionEnd, STALE_EVENT_LIMIT,
};
use crate::{
    file::StateConflict,
    timeout::{Deadline, Timeout},
    Error, Result,
};

/// A wait's deadline and its one bounded drain after expiry.
/// Reading expiry once per step keeps the native poll and settlement in agreement.
pub(in crate::file) enum DrainBudget {
    Ahead(Deadline),
    Relative { start: Instant, wait: Timeout },
    Late { polls: usize },
    Spent,
}

impl DrainBudget {
    pub(super) fn new(deadline: Deadline) -> Self {
        Self::Ahead(deadline)
    }

    /// Start cleanup once using the async close's original submission time.
    pub(super) fn cleanup(wait: Option<Timeout>, start: Instant) -> Self {
        wait.map_or(Self::Spent, |wait| {
            // At the clock's representable limit, spend the checked
            // duration using elapsed time instead of skipping cleanup.
            Deadline::starting(start, wait).map_or(Self::Relative { start, wait }, Self::new)
        })
    }

    pub(super) fn late() -> Self {
        Self::Late { polls: 0 }
    }

    pub(super) fn poll(&mut self, step: Duration, poll: impl FnOnce(u32) -> bool) -> Pumped {
        self.poll_at(step, Instant::now(), poll)
    }

    /// The poll and its result belong to the same step: only this operation
    /// can spend the budget, whether the late phase is empty or reaches its limit.
    pub(super) fn poll_at(
        &mut self,
        step: Duration,
        at: Instant,
        poll: impl FnOnce(u32) -> bool,
    ) -> Pumped {
        let expired = match self {
            Self::Ahead(deadline) => deadline.end() <= at,
            Self::Relative { start, wait } => wait.remaining_at(*start, at).is_zero(),
            Self::Late { .. } | Self::Spent => false,
        };
        if expired {
            *self = Self::late();
        }
        let (wait, late) = match self {
            Self::Ahead(deadline) => (deadline.step_ms_at(at, step), false),
            Self::Relative { start, wait } => {
                (wait.remaining_at(*start, at).min(step).millis(), false)
            }
            Self::Late { .. } => (0, true),
            Self::Spent => return Pumped::Exhausted,
        };
        let empty = poll(wait);
        if let Self::Late { polls } = self {
            *polls += 1;
            if empty || *polls == STALE_EVENT_LIMIT {
                *self = Self::Spent;
            }
        }
        if empty {
            Pumped::Empty { late }
        } else {
            Pumped::Took { late }
        }
    }
}

/// Evidence from one poll, or from exhausting its bounded final drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pumped {
    Took { late: bool },
    Empty { late: bool },
    Exhausted,
}

impl Pumped {
    pub(super) fn expired(self) -> bool {
        !matches!(
            self,
            Self::Took { late: false } | Self::Empty { late: false }
        )
    }

    fn drain_over(self) -> bool {
        !matches!(self, Self::Took { .. })
    }
}

/// Proof that the due batch completed its own bounded post-deadline drain.
/// Only the settlement kernel mints this proof, after its forced-drain loop.
#[derive(Clone, Copy)]
pub(in crate::file) struct LateDrain {
    _private: (),
}

#[cfg(test)]
impl LateDrain {
    pub(super) fn for_test() -> Self {
        Self { _private: () }
    }
}

/// One fixed settlement batch. Reply callbacks cannot advance its clock.
pub(in crate::file) struct Settlement {
    pub(in crate::file) cleanup_expired: bool,
    pub(in crate::file) at: Instant,
}

/// Completion evidence for either kind of registered waiter.
pub(in crate::file) enum WaitOutcome {
    Ended(SessionEnd),
    Expired(RecordingReport, LateDrain),
}

impl WaitOutcome {
    pub(in crate::file) fn into_finish(self) -> FinishStatus {
        match self {
            Self::Ended(end) => FinishStatus::Ended(end),
            Self::Expired(report, _) => FinishStatus::TimedOut(report),
        }
    }

    pub(in crate::file) fn into_end(self) -> Option<SessionEnd> {
        match self {
            Self::Ended(end) => Some(end),
            Self::Expired(..) => None,
        }
    }
}

/// How a registered finish wait returns its result. Synchronous waits use a
/// stack slot; async waits use their executor-independent reply.
pub(in crate::file) trait FinishReply {
    fn cancelled(&self) -> bool;
    fn complete(self, result: Result<WaitOutcome>);
}

/// A recording session belongs to exactly one native recorder generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionId {
    pub(super) recorder: u64,
    pub(super) session: u64,
}

struct FinishWaiter<R> {
    deadline: Deadline,
    session: SessionId,
    reply: R,
}

/// The finish registry lives beside the ledger whose evidence settles it.
pub(in crate::file) struct FinishWaiters<R> {
    waiters: Vec<FinishWaiter<R>>,
    // Captured in the same step that resolves waiters, including shutdown
    // initiated by an executor callback during reply delivery.
    notify_cleanup: bool,
}

impl<R: FinishReply> FinishWaiters<R> {
    pub(in crate::file) fn new() -> Self {
        Self {
            waiters: Vec::new(),
            notify_cleanup: false,
        }
    }

    pub(in crate::file) fn notifies_cleanup(&self) -> bool {
        self.notify_cleanup || !self.waiters.is_empty()
    }

    #[cfg(test)]
    pub(in crate::file) fn len(&self) -> usize {
        self.waiters.len()
    }

    #[cfg(test)]
    pub(in crate::file) fn capacity(&self) -> usize {
        self.waiters.capacity()
    }
    pub(in crate::file) fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }

    pub(in crate::file) fn retain(&mut self) {
        self.waiters.retain(|waiter| !waiter.reply.cancelled());
    }

    pub(in crate::file) fn register(
        &mut self,
        recorder: &FileRecorder,
        deadline: Deadline,
        reply: R,
    ) {
        if let Err(error) = recorder.check_wait() {
            reply.complete(Err(error));
        } else if !reply.cancelled() {
            self.waiters.push(FinishWaiter {
                deadline,
                session: recorder.session_id(),
                reply,
            });
        }
    }

    pub(in crate::file) fn close(&mut self) {
        self.fail(StateConflict::Closed);
    }

    pub(in crate::file) fn fail(&mut self, error: StateConflict) {
        for waiter in self.waiters.drain(..) {
            waiter.reply.complete(Err(Error::InvalidState(error)));
        }
    }

    pub(in crate::file) fn wait_timeout(&self, tick: Duration) -> Duration {
        self.waiters
            .iter()
            .fold(tick, |wait, waiter| wait.min(waiter.deadline.remaining()))
    }

    /// Fix the due batch before delivering any reply: executor callbacks may
    /// take arbitrary time, so later deadlines need a fresh drain next time.
    pub(in crate::file) fn settle_at(
        &mut self,
        recorder: &mut FileRecorder,
        batch: Settlement,
        sink: RecorderEventSink<'_>,
        closing: impl Fn() -> bool,
        mut cleanup: Option<&mut DrainBudget>,
    ) {
        let Settlement {
            mut cleanup_expired,
            at,
        } = batch;
        self.retain();
        if self.waiters.is_empty() {
            return;
        }
        self.notify_cleanup |= closing();
        let late = if self
            .waiters
            .iter()
            .any(|waiter| cleanup_expired || waiter.deadline.end() <= at)
        {
            // A pump poll before expiry is never evidence that the queue is
            // empty after expiry. Every newly due batch gets its own drain.
            let mut budget = DrainBudget::late();
            loop {
                let polled = if let Some(cleanup) = cleanup.as_deref_mut() {
                    // The local late phase and the final cleanup both account
                    // for this poll. An exhausted cleanup makes no native call.
                    budget.poll(Duration::ZERO, |_| {
                        recorder.pump(cleanup, Duration::ZERO, sink).drain_over()
                    })
                } else {
                    recorder.pump(&mut budget, Duration::ZERO, sink)
                };
                if polled.drain_over() {
                    break;
                }
            }
            // Cleanup can expire during this batch's drain. Its spent budget
            // now covers every waiter, including deadlines later than `at`.
            cleanup_expired |= cleanup
                .as_deref()
                .is_some_and(|budget| matches!(budget, DrainBudget::Spent));
            Some(LateDrain { _private: () })
        } else {
            None
        };
        let session = recorder.session_id();
        let mut index = 0;
        while index < self.waiters.len() {
            let waiter = &self.waiters[index];
            let outcome = if waiter.session != session {
                Some(Err(Error::InvalidState(StateConflict::Closed)))
            } else {
                recorder
                    .settle_wait(
                        late.as_ref()
                            .filter(|_| cleanup_expired || waiter.deadline.end() <= at),
                    )
                    .map(|outcome| {
                        if matches!(outcome, WaitOutcome::Expired(..)) && closing() {
                            Err(Error::InvalidState(StateConflict::Closed))
                        } else {
                            Ok(outcome)
                        }
                    })
            };
            if let Some(outcome) = outcome {
                self.waiters.swap_remove(index).reply.complete(outcome);
            } else {
                index += 1;
            }
        }
        self.notify_cleanup |= closing();
    }
}

pub(super) struct LocalFinish<'a>(pub(super) &'a std::cell::RefCell<Option<Result<WaitOutcome>>>);

impl FinishReply for LocalFinish<'_> {
    fn cancelled(&self) -> bool {
        false
    }
    fn complete(self, result: Result<WaitOutcome>) {
        *self.0.borrow_mut() = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::recorder::FINISH_POLL;
    use crate::file::recorder::SETTLE_POLL;

    #[test]
    fn checked_cleanup_waits_keep_their_budget_until_it_is_spent() {
        let start = Instant::now();
        for wait in [Duration::from_nanos(1), FINISH_POLL, crate::MAX_TIMEOUT] {
            let mut cleanup = Some(Timeout::new(wait).unwrap());
            let mut budget = DrainBudget::cleanup(cleanup.take(), start);
            assert!(!matches!(budget, DrainBudget::Spent));
            assert_eq!(
                budget.poll_at(FINISH_POLL, start, |millis| {
                    assert!(millis > 0);
                    false
                }),
                Pumped::Took { late: false },
            );
            assert!(matches!(
                DrainBudget::cleanup(cleanup.take(), start),
                DrainBudget::Spent
            ));
        }
    }

    #[test]
    fn an_unrepresentable_cleanup_end_spends_the_checked_wait_using_elapsed_time() {
        // Find an instant at the platform clock's upper boundary without
        // relying on a particular Instant representation.
        let now = Instant::now();
        let (mut low, mut high) = (0_u64, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2 + 1;
            if now.checked_add(Duration::from_secs(middle)).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let start = now.checked_add(Duration::from_secs(low)).unwrap();
        let wait = Timeout::new(Duration::from_secs(2)).unwrap();
        assert!(Deadline::starting(start, wait).is_err());
        let mut budget = DrainBudget::cleanup(Some(wait), start);
        assert!(matches!(budget, DrainBudget::Relative { .. }));
        assert_eq!(
            budget.poll_at(FINISH_POLL, start, |millis| {
                assert_eq!(millis, 25);
                false
            }),
            Pumped::Took { late: false },
        );

        // The same fallback remains bounded when the clock can advance.
        let mut budget = DrainBudget::Relative { start: now, wait };
        assert_eq!(
            budget.poll_at(FINISH_POLL, now + Duration::from_secs(2), |millis| {
                assert_eq!(millis, 0);
                true
            }),
            Pumped::Empty { late: true },
        );
        assert!(matches!(budget, DrainBudget::Spent));
    }

    #[test]
    fn every_drain_uses_one_expiry_read_and_one_bounded_late_phase() {
        let at = Instant::now();
        let deadline =
            Deadline::starting(at, Timeout::new(Duration::from_millis(7)).unwrap()).unwrap();
        let mut budget = DrainBudget::new(deadline);
        for (step, expected) in [(FINISH_POLL, 7), (SETTLE_POLL, 1)] {
            assert_eq!(
                budget.poll_at(step, at, |wait| {
                    assert_eq!(wait, expected);
                    false
                }),
                Pumped::Took { late: false }
            );
        }
        for _ in 0..STALE_EVENT_LIMIT {
            assert_eq!(
                budget.poll_at(FINISH_POLL, deadline.end(), |wait| {
                    assert_eq!(wait, 0);
                    false
                }),
                Pumped::Took { late: true }
            );
        }
        assert_eq!(
            budget.poll_at(FINISH_POLL, deadline.end(), |_| panic!(
                "spent budget polled"
            )),
            Pumped::Exhausted
        );
        assert_eq!(
            budget.poll_at(FINISH_POLL, at, |_| panic!("spent budget polled")),
            Pumped::Exhausted
        );
    }

    #[test]
    fn an_empty_late_poll_spends_the_budget_and_a_spent_budget_never_captures() {
        let mut budget = DrainBudget::late();
        assert_eq!(
            budget.poll(Duration::ZERO, |_| true),
            Pumped::Empty { late: true }
        );
        let device = crate::file::mock::Recorder::new(crate::file::mock::Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &crate::file::mock::recorder_api(),
            &crate::file::FileRecorderOptions::new(
                crate::file::SourceSpec::new("spent budget").unwrap(),
            ),
        )
        .unwrap();
        let before = device.captures.load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(
            recorder.pump(
                &mut budget,
                FINISH_POLL,
                Some(&|_| panic!("spent budget emitted")),
            ),
            Pumped::Exhausted
        );
        assert_eq!(
            device.captures.load(std::sync::atomic::Ordering::Acquire),
            before
        );
    }
}
