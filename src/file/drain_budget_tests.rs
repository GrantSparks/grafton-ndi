//! Run the same final-drain contract through synchronous and registered waits.
use super::*;
use crate::file::mock::{self, Closing};
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

#[derive(Clone, Copy, Debug)]
enum WaitPath {
    Synchronous,
    Registered,
}

impl WaitPath {
    fn finish(
        self,
        recorder: &mut FileRecorder,
        deadline: Deadline,
        earlier_empty: bool,
    ) -> FinishStatus {
        match self {
            Self::Synchronous => recorder.wait_until(deadline).unwrap(),
            Self::Registered => {
                let result = RefCell::new(None);
                let mut waiters = FinishWaiters::new();
                waiters.register(recorder, deadline, LocalFinish(&result));
                waiters.settle_at(
                    recorder,
                    Settlement {
                        drained: earlier_empty,
                        cleanup_expired: false,
                        at: deadline.end(),
                    },
                    &mut |_| {},
                    || false,
                    None,
                );
                result
                    .into_inner()
                    .expect("the expired wait was settled")
                    .unwrap()
            }
        }
    }
}

fn recording(device: &Arc<mock::Recorder>) -> FileRecorder {
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(SourceSpec::new("drain budget fixture").unwrap()),
    )
    .unwrap();
    let target =
        RecordingTarget::new(std::env::temp_dir().join("mock-drain-budget-paired")).unwrap();
    recorder
        .start(&target, RecordingTimecode::Preserve)
        .unwrap();
    recorder
}

#[test]
fn synchronous_and_registered_waits_share_the_exact_late_poll_limit() {
    for path in [WaitPath::Synchronous, WaitPath::Registered] {
        let device = mock::Recorder::new(Closing::Never);
        let mut recorder = recording(&device);
        for _ in 0..1024 {
            device.push(mock::FIRST_FRAME, mock::TAKE);
        }
        recorder.request_stop();
        let before = device.captures.load(Ordering::Acquire);
        let outcome = path.finish(
            &mut recorder,
            Deadline::after(Duration::ZERO).unwrap(),
            false,
        );
        assert!(matches!(outcome, FinishStatus::TimedOut(_)), "{path:?}");
        assert_eq!(
            device.captures.load(Ordering::Acquire) - before,
            STALE_EVENT_LIMIT,
            "{path:?}"
        );
        assert!(
            recorder.next_event(Duration::ZERO).unwrap().is_some(),
            "{path:?}: consumed the backlog"
        );
        recorder.abandon();
    }
}

#[test]
fn synchronous_and_registered_waits_redrain_a_closure_queued_after_an_earlier_empty_poll() {
    for path in [WaitPath::Synchronous, WaitPath::Registered] {
        let device = mock::Recorder::new(Closing::WhenTold);
        let mut recorder = recording(&device);
        recorder.request_stop();
        while recorder.next_event(Duration::ZERO).unwrap().is_some() {}
        // The first drain was empty before the wait expired. Closure becomes
        // queued before the deadline; expiry must observe it with a fresh drain.
        device.close();
        let deadline = Deadline::after(Duration::ZERO).unwrap();
        let before = device.captures.load(Ordering::Acquire);
        let outcome = path.finish(&mut recorder, deadline, true);
        assert!(
            matches!(outcome, FinishStatus::Closed(_)),
            "{path:?}: {outcome:?}"
        );
        assert_eq!(
            device.captures.load(Ordering::Acquire) - before,
            2,
            "{path:?}: closure and fresh empty poll"
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 1, "{path:?}");
    }
}

#[test]
fn a_relative_drain_keeps_checked_remainders_when_its_end_cannot_be_represented() {
    // Find a representable instant near the platform's upper edge without
    // assuming the representation or its epoch.
    let mut last = Instant::now();
    for bit in (0..64).rev() {
        if let Some(next) = last.checked_add(Duration::from_secs(1u64 << bit)) {
            last = next;
        }
    }
    for bit in (0..30).rev() {
        if let Some(next) = last.checked_add(Duration::from_nanos(1u64 << bit)) {
            last = next;
        }
    }
    let start = last - Duration::from_secs(1);
    let wait = Timeout::new(Duration::from_secs(2)).unwrap();
    assert!(start.checked_add(wait.duration()).is_none());
    let mut budget = DrainBudget::Relative { start, wait };
    let at = start + Duration::from_millis(250);
    let mut waits = Vec::new();
    budget.poll_at(Duration::MAX, at, |millis| {
        waits.push(millis);
        false
    });
    budget.poll_at(Duration::from_millis(100), at, |millis| {
        waits.push(millis);
        false
    });
    assert_eq!(waits, [1750, 100]);
}
