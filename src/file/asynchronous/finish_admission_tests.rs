use super::{
    super::mock::{self, Closing},
    tests::{block_on, block_owner, mock_recorder_with_drop_wait},
    *,
};
use std::{sync::Barrier, task::Wake};

fn recorder(capacity: usize) -> (Arc<mock::Recorder>, AsyncFileRecorder) {
    mock_recorder_with_drop_wait(
        Closing::Never,
        AsyncFileOptions::new().command_capacity(capacity),
        Duration::ZERO,
    )
}

fn permits(recorder: &AsyncFileRecorder) -> usize {
    let state = recorder
        .worker
        .finish_admission
        .state
        .load(Ordering::Acquire);
    assert_eq!(state & 1, 0, "finish waits never borrow the stop reserve");
    state >> 1
}

fn pending(recorder: &AsyncFileRecorder) -> usize {
    // A response may wake its consumer before the owner's next event pump.
    // Two commands ensure cancellation/settlement ran between the barriers.
    block_on(recorder.worker.call(|_| Ok(()))).unwrap();
    block_on(recorder.worker.call(|owner| Ok(owner.pending_finishes()))).unwrap()
}

fn wait_for_empty_commands(recorder: &AsyncFileRecorder) {
    let limit = Instant::now() + Duration::from_secs(5);
    while recorder.worker.admission.state.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < limit, "owner stopped processing commands");
        thread::yield_now();
    }
}

fn registered(recorder: &AsyncFileRecorder) {
    wait_for_empty_commands(recorder);
    assert!(pending(recorder) > 0);
}

#[test]
fn a_full_finish_budget_refuses_waits_but_preserves_status_stop_and_close() {
    let (device, recorder) = recorder(1);
    let finishing = recorder.finish(crate::MAX_TIMEOUT).submit();
    registered(&recorder);
    assert_eq!(permits(&recorder), 1);
    assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 0);

    assert!(matches!(
        block_on(recorder.finish(crate::MAX_TIMEOUT)),
        Err(Error::QueueFull(QueueLimit::FileFinishWaits))
    ));
    assert!(matches!(
        block_on(recorder.wait_for_finish(crate::MAX_TIMEOUT)),
        Err(Error::QueueFull(QueueLimit::FileFinishWaits))
    ));
    assert_eq!(pending(&recorder), 1);
    assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Stopping);
    assert_eq!(block_on(recorder.report()).unwrap().session(), 1);
    assert_eq!(
        block_on(recorder.request_stop()).unwrap(),
        StopRequest::AlreadyStopping
    );
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    block_on(recorder.close()).unwrap();
    assert!(matches!(
        block_on(finishing),
        Err(Error::InvalidState(StateConflict::Closed))
    ));
    assert_eq!(permits(&recorder), 0);
}

#[test]
fn cancelling_a_registered_wait_restores_its_separate_budget() {
    let (_, recorder) = recorder(1);
    let cancelled = recorder.finish(crate::MAX_TIMEOUT).submit();
    registered(&recorder);
    assert_eq!(permits(&recorder), 1);
    drop(cancelled);
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);

    let replacement = recorder.wait_for_finish(crate::MAX_TIMEOUT).submit();
    registered(&recorder);
    assert_eq!(permits(&recorder), 1);
    drop(replacement);
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    block_on(recorder.close()).unwrap();
}

#[test]
fn refused_state_and_normal_closure_each_restore_waiter_admission() {
    let (device, recorder) = mock_recorder_with_drop_wait(
        Closing::AtStop,
        AsyncFileOptions::new().command_capacity(1),
        Duration::ZERO,
    );
    assert!(matches!(
        block_on(recorder.wait_for_finish(crate::MAX_TIMEOUT)),
        Err(Error::InvalidState(StateConflict::StopNotRequested))
    ));
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    assert_eq!(device.stops.load(Ordering::Acquire), 0);

    assert!(matches!(
        block_on(recorder.finish(crate::MAX_TIMEOUT)).unwrap(),
        FinishStatus::Closed(_)
    ));
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    assert!(matches!(
        block_on(recorder.wait_for_finish(Duration::ZERO)).unwrap(),
        Some(FinishStatus::Closed(_))
    ));
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    block_on(recorder.close()).unwrap();
}

#[test]
fn abandon_bypasses_full_waiter_command_and_reserved_stop_budgets() {
    let (device, recorder) = recorder(1);
    let finishing = recorder.finish(crate::MAX_TIMEOUT).submit();
    registered(&recorder);
    let (blocked, release) = block_owner(&recorder.worker);
    let queued = recorder.state().submit();
    let stopping = recorder.request_stop().submit();
    assert_eq!(permits(&recorder), 1);
    assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
    let abandoned = recorder.abandon().submit();
    assert!(!abandoned.is_completed());
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    assert_eq!(block_on(abandoned).unwrap().session(), 1);
    for result in [
        block_on(queued).map(|_| ()),
        block_on(stopping).map(|_| ()),
        block_on(finishing).map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(Error::InvalidState(StateConflict::Closed))
        ));
    }
    assert_eq!(permits(&recorder), 0);
    assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 0);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn a_cancelled_queued_finish_holds_its_permit_until_its_stop_runs() {
    let (device, recorder) = recorder(1);
    let (blocked, release) = block_owner(&recorder.worker);
    let finishing = recorder.finish(crate::MAX_TIMEOUT).submit();
    assert_eq!(permits(&recorder), 1);
    drop(finishing);
    assert_eq!(permits(&recorder), 1);
    assert_eq!(device.stops.load(Ordering::Acquire), 0);

    // The independent waiter permit must not consume the reserved stop place.
    let stopping = recorder.request_stop().submit();
    assert!(!stopping.is_completed());
    assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 3);
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    assert_eq!(block_on(stopping).unwrap(), StopRequest::AlreadyStopping);
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    block_on(recorder.close()).unwrap();
}

#[test]
fn an_expired_queued_finish_releases_both_permits_after_stopping() {
    let (device, recorder) = recorder(1);
    let (blocked, release) = block_owner(&recorder.worker);
    let finishing = recorder.finish(Duration::ZERO).submit();
    assert_eq!(permits(&recorder), 1);
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    assert!(matches!(
        block_on(finishing).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    assert_eq!(recorder.worker.admission.state.load(Ordering::Acquire), 0);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert!(block_on(recorder.wait_for_finish(Duration::ZERO))
        .unwrap()
        .is_none());
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    block_on(recorder.close()).unwrap();
}

#[test]
fn finish_refusals_preserve_owner_timeout_and_command_error_precedence() {
    let (_, recorder) = recorder(1);
    let finishing = recorder.finish(crate::MAX_TIMEOUT).submit();
    registered(&recorder);
    let (blocked, release) = block_owner(&recorder.worker);
    let queued = recorder.state().submit();
    let stopping = recorder.request_stop().submit();
    let too_long = crate::MAX_TIMEOUT + Duration::from_nanos(1);

    assert!(matches!(
        block_on(recorder.finish(too_long)),
        Err(Error::InvalidInput(
            crate::InputDefect::TimeoutTooLong { .. }
        ))
    ));
    assert!(matches!(
        block_on(recorder.wait_for_finish(too_long)),
        Err(Error::InvalidInput(
            crate::InputDefect::TimeoutTooLong { .. }
        ))
    ));
    assert!(matches!(
        block_on(recorder.finish(Duration::ZERO)),
        Err(Error::QueueFull(QueueLimit::FileCommands))
    ));
    assert!(matches!(
        block_on(recorder.wait_for_finish(Duration::ZERO)),
        Err(Error::QueueFull(QueueLimit::FileCommands))
    ));
    assert_eq!(permits(&recorder), 1);

    let closing = recorder.close().submit();
    for refused in [
        recorder.finish(too_long).submit(),
        recorder.finish(Duration::ZERO).submit(),
    ] {
        assert!(matches!(
            block_on(refused),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
    }
    assert!(matches!(
        block_on(recorder.wait_for_finish(too_long)),
        Err(Error::InvalidState(StateConflict::Closed))
    ));
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    block_on(closing).unwrap();
    for result in [
        block_on(queued).map(|_| ()),
        block_on(stopping).map(|_| ()),
        block_on(finishing).map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(Error::InvalidState(StateConflict::Closed))
        ));
    }
    assert_eq!(permits(&recorder), 0);

    let (_, failed) = self::recorder(1);
    let finishing = failed.finish(crate::MAX_TIMEOUT).submit();
    registered(&failed);
    assert!(matches!(
        block_on(
            failed
                .worker
                .call::<()>(|_| panic!("finish admission owner failure"))
        ),
        Err(Error::InvalidState(StateConflict::OwnerFailed))
    ));
    // The panicking command's fallback reply can run during unwind, before
    // the catch boundary publishes failure. Registered work fails afterwards.
    assert!(matches!(
        block_on(finishing),
        Err(Error::InvalidState(StateConflict::OwnerFailed))
    ));
    assert!(matches!(
        block_on(failed.finish(too_long)),
        Err(Error::InvalidState(StateConflict::OwnerFailed))
    ));
    assert!(matches!(
        block_on(failed.wait_for_finish(too_long)),
        Err(Error::InvalidState(StateConflict::OwnerFailed))
    ));
    assert!(matches!(
        block_on(failed.close()),
        Err(Error::InvalidState(StateConflict::OwnerFailed))
    ));
    assert_eq!(permits(&failed), 0);
}

#[test]
fn cloned_recorders_share_one_budget_for_queued_and_registered_waits() {
    const CAPACITY: usize = 4;
    const SUBMITTERS: usize = 16;
    let (_, recorder) = recorder(CAPACITY);
    let (blocked, release) = block_owner(&recorder.worker);
    let barrier = Arc::new(Barrier::new(SUBMITTERS));
    let submitted = thread::scope(|scope| {
        let threads: Vec<_> = (0..SUBMITTERS)
            .map(|_| {
                let recorder = recorder.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    recorder.finish(crate::MAX_TIMEOUT).submit()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut accepted = Vec::new();
    for finishing in submitted {
        if finishing.is_completed() {
            assert!(matches!(
                block_on(finishing),
                Err(Error::QueueFull(QueueLimit::FileFinishWaits))
            ));
        } else {
            accepted.push(finishing);
        }
    }
    assert_eq!(accepted.len(), CAPACITY);
    assert_eq!(permits(&recorder), CAPACITY);
    assert_eq!(
        recorder.worker.admission.state.load(Ordering::Acquire) >> 1,
        CAPACITY
    );
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    wait_for_empty_commands(&recorder);
    assert_eq!(pending(&recorder), CAPACITY);
    assert_eq!(permits(&recorder), CAPACITY);
    drop(accepted);
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    block_on(recorder.close()).unwrap();
}

#[test]
fn a_settling_reply_holds_its_permit_across_reentrant_executor_callbacks() {
    struct Retry {
        recorder: AsyncFileRecorder,
        result: Sender<(bool, FileOperation<RecorderState>)>,
    }
    impl Wake for Retry {
        fn wake(self: Arc<Self>) {
            let retry = self.recorder.finish(crate::MAX_TIMEOUT).submit();
            let refused = retry.is_completed()
                && matches!(
                    block_on(retry),
                    Err(Error::QueueFull(QueueLimit::FileFinishWaits))
                );
            self.result
                .send((refused, self.recorder.state().submit()))
                .unwrap();
        }
    }
    let (_, recorder) = recorder(1);
    let (blocked, release) = block_owner(&recorder.worker);
    let mut finishing = Box::pin(recorder.finish(Duration::ZERO).submit());
    let (result, response) = mpsc::channel();
    let waker = Waker::from(Arc::new(Retry {
        recorder: recorder.clone(),
        result,
    }));
    assert!(finishing
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    release.send(()).unwrap();
    block_on(blocked).unwrap();
    let (refused, status) = response.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        refused,
        "reply delivery released its permit before the callback returned"
    );
    assert_eq!(block_on(status).unwrap(), RecorderState::Stopping);
    assert!(matches!(
        block_on(finishing).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    assert_eq!(pending(&recorder), 0);
    assert_eq!(permits(&recorder), 0);
    block_on(recorder.close()).unwrap();
}
