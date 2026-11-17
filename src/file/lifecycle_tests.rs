//! The recorder's and player's lifecycle against stand-in SDK entry points.

use super::{
    mock::{self, Closing, FILE_CLOSED, ROLLOVER, TAKE},
    *,
};
use crate::{sys, timeout::Deadline, MediaTypes};
use std::{
    cell::Cell,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

/// A recorder with a session started on `device`.
fn recording_on(device: &Arc<mock::Recorder>) -> FileRecorder {
    recording_with(
        device,
        FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
}

fn recording_with(device: &Arc<mock::Recorder>, options: FileRecorderOptions) -> FileRecorder {
    device.install();
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap();
    recorder
        .start(&target("first"), RecordingTimecode::Preserve)
        .unwrap();
    recorder
}

fn target(name: &str) -> RecordingTarget {
    RecordingTarget::new(std::env::temp_dir().join(format!("mock-not-created-{name}"))).unwrap()
}

#[test]
fn an_active_or_stopping_session_refuses_start_until_its_known_file_closes() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    for stopping in [false, true] {
        if stopping {
            recorder.request_stop();
        }
        assert!(matches!(
            recorder.start(&target("active-start"), RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::PreviousSessionOpen))
        ));
    }
    device.close();
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn generated_session_timecodes_reach_the_native_start_unchanged() {
    for ticks in [0, 360_000_000_000] {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        let options = FileRecorderOptions::new(SourceSpec::new("timecode source").unwrap());
        let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap();
        let start = SessionTimecode::new(crate::Timecode::from_ticks(ticks)).unwrap();
        recorder
            .start(
                &target("generated-timecode"),
                RecordingTimecode::GenerateFrom(start),
            )
            .unwrap();
        assert_eq!(*device.start_timecodes.lock().unwrap(), [ticks]);
        assert_eq!(device.starts.load(Ordering::Acquire), 1);
        assert!(matches!(
            recorder.finish(Duration::ZERO).unwrap(),
            FinishStatus::Closed(_)
        ));
    }
}

#[test]
fn every_session_exit_issues_exactly_one_native_stop() {
    for ending in 0..4 {
        let device = mock::Recorder::new(Closing::AtStop);
        let mut recorder = recording_with(
            &device,
            FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
                .drop_wait(Duration::ZERO),
        );
        recorder.next_event(Duration::ZERO).unwrap();
        match ending {
            0 => {
                recorder.request_stop();
            }
            1 => device.end_session_itself(),
            2 => device.push(mock::ERROR, mock::TAKE),
            3 => {
                device.dropped_events.store(1, Ordering::Release);
                device.close();
            }
            _ => unreachable!(),
        }
        while recorder.next_event(Duration::ZERO).unwrap().is_some() {}
        assert!(matches!(
            recorder.state(),
            RecorderState::Closed | RecorderState::Indeterminate
        ));
        assert_eq!(device.stops.load(Ordering::Acquire), 1, "ending {ending}");
        recorder.request_stop();
        drop(recorder);
        assert_eq!(device.stops.load(Ordering::Acquire), 1, "ending {ending}");
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }
}

#[test]
fn a_timeout_keeps_the_recorder_and_a_later_close_completes_without_a_second_stop() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    let report = recorder.report();
    assert!(recorder.wait_for_finish(Duration::ZERO).unwrap().is_none());
    assert_eq!(recorder.report(), report);
    assert!(recorder
        .wait_for_finish(Duration::from_millis(30))
        .unwrap()
        .is_none());
    assert_eq!(device.destroys.load(Ordering::Acquire), 0);
    device.push(FILE_CLOSED, TAKE);
    assert!(matches!(
        recorder.wait_for_finish(Duration::from_secs(1)).unwrap(),
        Some(FinishStatus::Closed(_))
    ));
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyClosed);
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn finish_waits_for_a_close_that_follows_the_stop() {
    let device = mock::Recorder::new(Closing::After(Duration::from_millis(100)));
    let mut recorder = recording_on(&device);
    let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(5)).unwrap() else {
        panic!("the close arrived within the wait");
    };
    assert!(report.closure_confirmed());
    assert_eq!(report.closed_files(), 1);
    assert_eq!(
        report.last_closed_path().unwrap().as_bytes(),
        TAKE.as_bytes()
    );
}

#[test]
fn a_rollover_requested_just_before_the_stop_does_not_hold_the_session_open() {
    // The SDK accepts the rollover, then the stop arrives before the cut: only
    // the file that was open is reported closed.
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    recorder.rollover(&target("second")).unwrap();
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Closed(_)),
        "the session never reached a final state: {outcome:?}"
    );
}

#[test]
fn a_source_switch_just_before_the_stop_does_not_hold_the_session_open() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    recorder
        .set_source(&SourceSpec::new("another source").unwrap())
        .unwrap();
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn a_completed_rollover_is_followed_in_the_order_the_sdk_reports_it() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    recorder.rollover(&target("second")).unwrap();
    // The next file is reported open before the previous one is reported closed.
    device.push(ROLLOVER, "second.mkv");
    device.push(FILE_CLOSED, TAKE);
    while recorder.next_event(Duration::ZERO).unwrap().is_some() {}
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert_eq!(
        recorder.reported_current_file().unwrap().as_bytes(),
        b"second.mkv"
    );
    recorder.request_stop();
    device.push(FILE_CLOSED, "second.mkv");
    let Some(FinishStatus::Closed(report)) =
        recorder.wait_for_finish(Duration::from_secs(1)).unwrap()
    else {
        panic!("both files were reported closed");
    };
    assert_eq!(report.closed_files(), 2);
}

#[test]
fn a_session_whose_close_is_never_reported_times_out_rather_than_claiming_closure() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    let outcome = recorder.finish(Duration::from_millis(30)).unwrap();
    assert!(matches!(outcome, FinishStatus::TimedOut(_)), "{outcome:?}");
    assert!(!recorder.abandon().closure_confirmed());
}

#[test]
fn resetting_an_unconfirmed_session_preserves_its_report_and_isolates_late_events() {
    let old = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&old);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::TimedOut(_)
    ));

    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.install();
    let report = recorder
        .reset(&FileRecorderOptions::new(
            SourceSpec::new("replacement").unwrap(),
        ))
        .unwrap();
    assert_eq!(report.session(), 1);
    assert!(!report.closure_confirmed());
    assert_eq!(old.destroys.load(Ordering::Acquire), 1);
    assert_eq!(recorder.state(), RecorderState::Idle);
    assert_eq!(recorder.report().session(), report.session());
    // Even a close reported after replacement belongs only to the destroyed
    // native instance. It must never close the replacement's new session.
    old.push(FILE_CLOSED, TAKE);
    let session = recorder
        .start(&target("replacement"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(session, report.session() + 1);
    take_events(&mut recorder);
    assert_eq!(recorder.report().closed_files(), 0);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
    drop(recorder);
    assert_eq!(replacement.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn a_failed_reset_keeps_the_old_recorder_and_its_finalization_pending() {
    let old = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&old);
    recorder.finish(Duration::ZERO).unwrap();
    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.refuse_creation.store(true, Ordering::Release);
    replacement.install();
    assert!(matches!(
        recorder.reset(&FileRecorderOptions::new(
            SourceSpec::new("replacement").unwrap()
        )),
        Err(Error::NativeOperationFailed {
            operation}) if operation.name() == c"NDIlib_file_recorder_create"
    ));
    assert_eq!(old.destroys.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Stopping);
    old.push(FILE_CLOSED, TAKE);
    assert!(matches!(
        recorder.wait_for_finish(Duration::from_secs(1)).unwrap(),
        Some(FinishStatus::Closed(_))
    ));
}

#[test]
fn an_indeterminate_report_counts_only_files_confirmed_empty_before_loss() {
    for empty_before_loss in [false, true] {
        let device = mock::Recorder::new(Closing::Never);
        let mut recorder = recording_with(
            &device,
            FileRecorderOptions::new(SourceSpec::new("empty-count").unwrap())
                .drop_wait(Duration::ZERO),
        );
        recorder.next_event(Duration::ZERO).unwrap();
        if empty_before_loss {
            device.push(FILE_CLOSED, TAKE);
            recorder.next_event(Duration::ZERO).unwrap();
            assert_eq!(recorder.report().empty_files(), 1);
        }
        device.dropped_events.store(1, Ordering::Release);
        if !empty_before_loss {
            device.push(FILE_CLOSED, TAKE);
        }
        device.push(ROLLOVER, "hidden.mkv");
        device.push(FILE_CLOSED, "hidden.mkv");
        let FinishStatus::Indeterminate(report) = recorder.finish(Duration::ZERO).unwrap() else {
            panic!("lost file evidence prevents confirmed closure");
        };
        assert_eq!(report.empty_files(), u64::from(empty_before_loss));
        assert_eq!(report.closed_files(), 2);
        assert_eq!(report.dropped_events(), 1);
        recorder.abandon();
    }
}

#[test]
fn lost_sdk_events_end_the_session_as_indeterminate() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.dropped_events.store(1, Ordering::Release);
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Indeterminate(_)),
        "{outcome:?}"
    );
    assert!(!recorder.report().closure_confirmed());
    assert_eq!(recorder.report().dropped_events(), 1);
    let Some(FinishStatus::Indeterminate(report)) =
        recorder.wait_for_finish(Duration::ZERO).unwrap()
    else {
        panic!("a settled session must remain an indeterminate outcome");
    };
    assert_eq!(report, recorder.report());
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyEnded);
    // Lost lifecycle events can hide an open file, even after every known
    // file closed. Only a new native recorder can start another session.
    assert!(matches!(
        recorder.start(&target("again"), RecordingTimecode::Preserve),
        Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
    ));
    assert_eq!(device.starts.load(Ordering::Acquire), 1);
    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.install();
    assert_eq!(
        recorder
            .reset(&FileRecorderOptions::new(
                SourceSpec::new("replacement").unwrap(),
            ))
            .unwrap(),
        report
    );
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    recorder
        .start(&target("again"), RecordingTimecode::Preserve)
        .unwrap();
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::Closed(_)
    ));
    drop(recorder);
    assert_eq!(replacement.stops.load(Ordering::Acquire), 1);
    assert_eq!(replacement.destroys.load(Ordering::Acquire), 1);
}

/// Lose a rollover's open event and close the one file the wrapper observed.
fn recording_with_a_lost_open(
    device: &Arc<mock::Recorder>,
    wait: Duration,
) -> (FileRecorder, RecordingReport) {
    let mut recorder = recording_with(
        device,
        FileRecorderOptions::new(SourceSpec::new("lost open source").unwrap()).drop_wait(wait),
    );
    assert_eq!(
        recorder.next_event(Duration::ZERO).unwrap().unwrap().kind(),
        RecorderEventKind::Started
    );
    device.dropped_events.store(1, Ordering::Release);
    device.push(FILE_CLOSED, TAKE);
    let FinishStatus::Indeterminate(report) = recorder.finish(Duration::ZERO).unwrap() else {
        panic!("the lost open event prevents confirmation of closure");
    };
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.dropped_events(), 1);
    assert!(recorder.reported_current_file().is_none());
    assert!(!recorder.is_recording());
    (recorder, report)
}

#[test]
fn a_lost_open_still_blocks_restart_after_all_observed_and_hidden_files_close() {
    let device = mock::Recorder::new(Closing::Never);
    let (mut recorder, report) = recording_with_a_lost_open(&device, Duration::ZERO);
    for path in ["hidden.mkv", "another-hidden.mkv"] {
        device.push(FILE_CLOSED, path);
        let event = recorder.next_event(Duration::ZERO).unwrap().unwrap();
        assert_eq!(event.kind(), RecorderEventKind::FileClosed);
        assert_eq!(event.path().as_bytes(), path.as_bytes());
        assert!(recorder.next_event(Duration::ZERO).unwrap().is_none());
        assert_eq!(recorder.report(), report);
        assert!(matches!(
            recorder.start(&target("restart-after-loss"), RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        assert_eq!(device.starts.load(Ordering::Acquire), 1);
    }
    assert_eq!(recorder.abandon(), report);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn drop_spends_its_remaining_budget_after_a_lost_open_even_when_hidden_closes_arrive() {
    for delayed in [false, true] {
        // Give the worker a generous scheduling window on loaded CI runners;
        // the queued case needs no cross-thread event delivery.
        let budget = if delayed {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(80)
        };
        let device = mock::Recorder::new(Closing::Never);
        let (recorder, _) = recording_with_a_lost_open(&device, budget);
        let captures = device.captures.load(Ordering::Acquire);
        let waits = device.waits.load(Ordering::Acquire);
        let closer = if delayed {
            let device = Arc::clone(&device);
            let (ready, started) = std::sync::mpsc::channel();
            let closer = std::thread::spawn(move || {
                ready.send(()).unwrap();
                // Wait until cleanup is actually polling, rather than relying
                // on the scheduler to deliver a close after a fixed sleep.
                let deadline = Instant::now() + Duration::from_secs(5);
                while device.waits.load(Ordering::Acquire) == waits
                    && device.destroys.load(Ordering::Acquire) == 0
                {
                    assert!(Instant::now() < deadline, "cleanup never began waiting");
                    std::thread::yield_now();
                }
                assert_eq!(device.destroys.load(Ordering::Acquire), 0);
                device.push(FILE_CLOSED, "hidden.mkv");
            });
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            Some(closer)
        } else {
            device.push(FILE_CLOSED, "hidden.mkv");
            None
        };
        let began = Instant::now();
        drop(recorder);
        assert!(
            began.elapsed() >= budget,
            "known-empty incomplete tracking skipped the cleanup budget"
        );
        if let Some(closer) = closer {
            closer.join().unwrap();
        }
        assert!(device.captures.load(Ordering::Acquire) > captures);
        assert!(device.waits.load(Ordering::Acquire) > waits);
        assert_eq!(device.starts.load(Ordering::Acquire), 1);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }
}

#[test]
fn a_failed_reset_preserves_the_unknown_file_cleanup_obligation() {
    let device = mock::Recorder::new(Closing::Never);
    let (mut recorder, report) = recording_with_a_lost_open(&device, Duration::ZERO);
    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.refuse_creation.store(true, Ordering::Release);
    replacement.install();
    assert!(matches!(
        recorder.reset(&FileRecorderOptions::new(
            SourceSpec::new("replacement").unwrap(),
        )),
        Err(Error::NativeOperationFailed { operation })
            if operation.name() == c"NDIlib_file_recorder_create"
    ));
    assert_eq!(device.destroys.load(Ordering::Acquire), 0);
    device.push(FILE_CLOSED, "hidden.mkv");
    recorder.next_event(Duration::ZERO).unwrap().unwrap();
    assert_eq!(recorder.report(), report);
    assert!(matches!(
        recorder.start(
            &target("restart-after-failed-reset"),
            RecordingTimecode::Preserve
        ),
        Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
    ));
    assert_eq!(device.starts.load(Ordering::Acquire), 1);
    assert_eq!(recorder.abandon(), report);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn zero_budget_drop_drains_hidden_closes_but_abandon_does_not_capture_them() {
    for abandon in [false, true] {
        let device = mock::Recorder::new(Closing::Never);
        let (recorder, report) = recording_with_a_lost_open(&device, Duration::ZERO);
        device.push(FILE_CLOSED, "hidden.mkv");
        let captures = device.captures.load(Ordering::Acquire);
        let waits = device.waits.load(Ordering::Acquire);
        if abandon {
            assert_eq!(recorder.abandon(), report);
            assert_eq!(device.captures.load(Ordering::Acquire), captures);
        } else {
            drop(recorder);
            assert!(device.captures.load(Ordering::Acquire) > captures);
        }
        assert_eq!(device.waits.load(Ordering::Acquire), waits);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }
}

#[test]
fn incomplete_tracking_uses_the_original_drop_budget_even_when_native_stop_is_slow() {
    let device = mock::Recorder::new(Closing::DuringSlowStop(Duration::from_millis(80)));
    let mut recorder = recording_with(
        &device,
        FileRecorderOptions::new(SourceSpec::new("lost open source").unwrap())
            .drop_wait(Duration::from_millis(40)),
    );
    recorder.next_event(Duration::ZERO).unwrap().unwrap();
    device.dropped_events.store(1, Ordering::Release);
    // Observe the loss while recording is still active. Drop must count its
    // subsequent native stop against the same cleanup budget as event waits.
    assert!(recorder.next_event(Duration::ZERO).unwrap().is_none());
    let waits = device.waits.load(Ordering::Acquire);
    drop(recorder);
    assert_eq!(device.waits.load(Ordering::Acquire), waits);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    assert!(device.closed_before_destroy.load(Ordering::Acquire));
}

#[test]
fn complete_tracking_needs_no_cleanup_after_every_file_is_confirmed_closed() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::Closed(_)
    ));
    device.push(mock::OPEN_FAILED, "outside-the-session.mkv");
    let captures = device.captures.load(Ordering::Acquire);
    drop(recorder);
    assert_eq!(device.captures.load(Ordering::Acquire), captures);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn evidence_lost_during_the_stale_event_drain_refuses_the_next_native_start() {
    for evidence in 0..3 {
        let device = mock::Recorder::new(Closing::AtStop);
        let mut recorder = recording_on(&device);
        assert!(matches!(
            recorder.finish(Duration::ZERO).unwrap(),
            FinishStatus::Closed(_)
        ));
        let report = recorder.report();
        match evidence {
            0 => device.push(ROLLOVER, "late-open.mkv"),
            1 => device.dropped_events.store(1, Ordering::Release),
            2 => {
                device.push(ROLLOVER, "unreadable-open.mkv");
                device.oversize_next_event.store(true, Ordering::Release);
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            recorder.start(
                &target("restart-after-late-evidence"),
                RecordingTimecode::Preserve
            ),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        assert_eq!(device.starts.load(Ordering::Acquire), 1);
        assert_eq!(recorder.report(), report, "final reports stay immutable");
        assert_eq!(
            recorder.finish(Duration::ZERO).unwrap(),
            FinishStatus::Closed(report.clone())
        );
        assert!(matches!(
            recorder.start(&target("restart-after-finish"), RecordingTimecode::Preserve),
            Err(Error::InvalidState(StateConflict::IncompleteFileTracking))
        ));
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.install();
        assert_eq!(
            recorder
                .reset(&FileRecorderOptions::new(
                    SourceSpec::new("replacement").unwrap(),
                ))
                .unwrap(),
            report
        );
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
        assert_eq!(
            recorder
                .start(
                    &target("recovered-after-late-evidence"),
                    RecordingTimecode::Preserve
                )
                .unwrap(),
            2
        );
        assert!(matches!(
            recorder.finish(Duration::ZERO).unwrap(),
            FinishStatus::Closed(_)
        ));
        drop(recorder);
        assert_eq!(replacement.stops.load(Ordering::Acquire), 1);
        assert_eq!(replacement.destroys.load(Ordering::Acquire), 1);
    }
}

#[test]
fn a_malformed_close_event_still_closes_the_session() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    recorder.request_stop();
    let mut event = mock::recorder_event(FILE_CLOSED, TAKE);
    event.source_format_payload.video_track_count = 3;
    device.push_event(event);
    let mut closed = None;
    while let Some(event) = recorder.next_event(Duration::ZERO).unwrap() {
        if event.kind() == RecorderEventKind::FileClosed {
            closed = Some(event);
        }
    }
    let event = closed.expect("the format must not hide the close event");
    assert_eq!(event.path().to_str().unwrap(), TAKE);
    assert_eq!(
        event.source_format(),
        Err(crate::SdkResultDefect::FileTrackCount {
            count: 3,
            capacity: 2
        })
    );
    assert!(matches!(
        recorder.wait_for_finish(Duration::from_millis(50)).unwrap(),
        Some(FinishStatus::Closed(_))
    ));
}

#[test]
fn an_unreadable_player_format_preserves_its_ended_event_and_probe_information() {
    let api = FileApi::with_tables(mock::playback_with_unreadable_formats(), mock::recording());
    let mut player =
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("mock player")).unwrap();
    let event = player.next_event(Duration::ZERO).unwrap().unwrap();
    assert_eq!(event.kind(), PlayerEventKind::Ended);
    assert_eq!(event.path().to_str().unwrap(), "p");
    assert_eq!(event.first_pts().ticks(), 42);
    assert_eq!(
        event.source_format(),
        Err(crate::SdkResultDefect::NegativeFormatCount {
            what: "audio sample count",
            value: -1
        })
    );
    let info = crate::file::FilePlayer::probe(&api, "mock.mkv").unwrap();
    assert_eq!(info.duration().value().ticks(), 123);
    assert!(info.duration().is_final());
    assert_eq!(info.track_count(), 4);
    assert_eq!(
        info.source_format(),
        Err(crate::SdkResultDefect::FileTrackCount {
            count: 3,
            capacity: 2
        })
    );
}

#[test]
fn an_event_larger_than_this_crate_expects_is_rejected_and_taints_the_session() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.oversize_next_event.store(true, Ordering::Release);
    let error = recorder.next_event(Duration::ZERO).unwrap_err();
    assert!(
        matches!(error,
        Error::UnexpectedSdkResult(crate::SdkResultDefect::FileStructureTooLarge { bytes, name })
        if bytes == std::mem::size_of::<sys::bindings::NDIlib_file_recorder_event_t>()
            && name == std::any::type_name::<sys::bindings::NDIlib_file_recorder_event_t>()),
        "{error}"
    );
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Indeterminate(_)),
        "{outcome:?}"
    );
    recorder.abandon();
}

/// An event of `kind` for the session's file whose format payload this crate
/// cannot decode.
fn malformed(
    kind: sys::bindings::NDIlib_file_recorder_event_type_e,
) -> sys::bindings::NDIlib_file_recorder_event_t {
    let mut event = mock::recorder_event(kind, TAKE);
    event.source_format_payload.video_track_count = 3;
    event
}

#[test]
fn a_malformed_close_taken_by_finish_still_closes_the_session() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    // Queued before the wait, so the wait itself takes it.
    device.push_event(malformed(FILE_CLOSED));
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    let FinishStatus::Closed(report) = outcome else {
        panic!("the close was taken, malformed or not: {outcome:?}");
    };
    assert!(report.closure_confirmed());
    assert_eq!(report.closed_files(), 1);
}

#[test]
fn a_malformed_event_during_finish_does_not_end_the_wait_for_the_close() {
    // The close comes well after the malformed event, which must not end the
    // wait with an error.
    let device = mock::Recorder::new(Closing::After(Duration::from_millis(100)));
    let mut recorder = recording_on(&device);
    device.push_event(malformed(mock::FIRST_FRAME));
    let outcome = recorder.finish(Duration::from_secs(5)).unwrap();
    let FinishStatus::Closed(report) = outcome else {
        panic!("the wait ended before the close: {outcome:?}");
    };
    // What the malformed event showed was kept: the file had a first frame.
    assert_eq!(report.empty_files(), 0);
    assert_eq!(report.closed_files(), 1);
}

#[test]
fn an_unreadable_event_during_finish_makes_the_outcome_indeterminate() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    // The queued start event is the one the wait cannot read.
    device.oversize_next_event.store(true, Ordering::Release);
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Indeterminate(_)),
        "{outcome:?}"
    );
    assert!(!recorder.report().closure_confirmed());
    recorder.abandon();
}

#[test]
fn dropping_a_recorder_with_a_malformed_event_queued_still_waits_for_its_close() {
    let device = mock::Recorder::new(Closing::After(Duration::from_millis(100)));
    let recorder = recording_on(&device);
    device.push_event(malformed(mock::FIRST_FRAME));
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    assert!(
        device.closed_before_destroy.load(Ordering::Acquire),
        "the malformed event ended the drop wait before the file was reported closed"
    );
}

#[test]
fn dropping_a_recording_recorder_waits_for_its_file_to_be_closed() {
    let device = mock::Recorder::new(Closing::After(Duration::from_millis(100)));
    let recorder = recording_on(&device);
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    assert!(
        device.closed_before_destroy.load(Ordering::Acquire),
        "the recorder was destroyed before its file was reported closed"
    );
}

#[test]
fn the_drop_wait_is_bounded_when_the_close_never_comes() {
    let device = mock::Recorder::new(Closing::Never);
    let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
        .drop_wait(Duration::from_millis(60));
    let recorder = recording_with(&device, options);
    let begun = Instant::now();
    drop(recorder);
    let waited = begun.elapsed();
    assert!(waited >= Duration::from_millis(60), "{waited:?}");
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    assert!(!device.closed_before_destroy.load(Ordering::Acquire));
}

#[test]
fn abandoning_stops_and_destroys_without_waiting() {
    let device = mock::Recorder::new(Closing::Never);
    let recorder = recording_on(&device);
    let begun = Instant::now();
    let report = recorder.abandon();
    assert!(begun.elapsed() < Duration::from_secs(1));
    assert!(!report.closure_confirmed());
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn a_recorder_that_never_started_is_destroyed_without_a_stop() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
    .unwrap();
    assert_eq!(recorder.report().session(), 0);
    assert_eq!(recorder.request_stop(), StopRequest::NotStarted);
    assert!(matches!(
        recorder.finish(Duration::ZERO),
        Err(Error::InvalidState(StateConflict::StopNotRequested))
    ));
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 0);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn a_target_in_a_directory_that_does_not_exist_is_refused_before_the_sdk_sees_it() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
    .unwrap();
    let missing = || {
        RecordingTarget::new(std::env::temp_dir().join("grafton-ndi-no-such-directory/take"))
            .unwrap()
    };
    let not_found = |result: Result<()>| match result {
        Err(Error::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected the directory to be reported missing: {other:?}"),
    };

    not_found(
        recorder
            .start(&missing(), RecordingTimecode::Preserve)
            .map(drop),
    );
    assert_eq!(recorder.state(), RecorderState::Idle);

    recorder
        .start(&target("first"), RecordingTimecode::Preserve)
        .unwrap();
    // The SDK would accept this rollover and then end the session.
    not_found(recorder.rollover(&missing()));
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    recorder.rollover(&target("second")).unwrap();
    assert_eq!(device.rollovers.load(Ordering::Acquire), 1);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn a_file_closed_before_its_first_frame_is_reported_as_empty() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    // The first file gets a frame; the one rolled over to never does.
    device.push(mock::FIRST_FRAME, TAKE);
    device.push(ROLLOVER, "second.mkv");
    device.push(FILE_CLOSED, TAKE);
    take_events(&mut recorder);
    assert_eq!(recorder.report().empty_files(), 0);
    recorder.request_stop();
    device.push(FILE_CLOSED, "second.mkv");
    let Some(FinishStatus::Closed(report)) =
        recorder.wait_for_finish(Duration::from_secs(1)).unwrap()
    else {
        panic!("both files were reported closed");
    };
    assert_eq!(report.closed_files(), 2);
    assert_eq!(report.empty_files(), 1);
}

#[test]
fn a_metadata_only_recording_reports_no_empty_files() {
    // The SDK reports no first frame for metadata, so its absence says nothing.
    let device = mock::Recorder::new(Closing::AtStop);
    let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
        .tracks(MediaTypes::METADATA);
    let mut recorder = recording_with(&device, options);
    let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(1)).unwrap() else {
        panic!("the file was reported closed");
    };
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.empty_files(), 0);
}

thread_local! {
    static PLAYER_CREATES: Cell<usize> = const { Cell::new(0) };
    static PLAYER_STOPS: Cell<usize> = const { Cell::new(0) };
    static PLAYER_DESTROYS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn create_player(
    _: *const sys::bindings::NDIlib_file_player_create_t,
) -> sys::bindings::NDIlib_file_player_instance_t {
    PLAYER_CREATES.set(PLAYER_CREATES.get() + 1);
    std::ptr::NonNull::dangling().as_ptr()
}
unsafe extern "C" fn stop_player(_: sys::bindings::NDIlib_file_player_instance_t) {
    PLAYER_STOPS.set(PLAYER_STOPS.get() + 1);
}
unsafe extern "C" fn destroy_player(_: sys::bindings::NDIlib_file_player_instance_t) {
    PLAYER_DESTROYS.set(PLAYER_DESTROYS.get() + 1);
}

#[test]
fn dropping_a_player_stops_then_destroys_it_once() {
    let api = FileApi::with_tables(
        sys::FilePlayback {
            NDIlib_file_player_create: create_player,
            NDIlib_file_player_stop: stop_player,
            NDIlib_file_player_destroy: destroy_player,
            ..mock::playback()
        },
        mock::recording(),
    );
    let player =
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("mock player")).unwrap();
    drop(player);
    assert_eq!(PLAYER_STOPS.get(), 1);
    assert_eq!(PLAYER_DESTROYS.get(), 1);
}

#[test]
fn malformed_player_options_are_refused_before_the_sdk_creates_a_player() {
    let api = FileApi::with_tables(
        sys::FilePlayback {
            NDIlib_file_player_create: create_player,
            ..mock::playback()
        },
        mock::recording(),
    );
    assert!(matches!(
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("")),
        Err(Error::InvalidInput(crate::InputDefect::Empty {
            what: "player name"
        }))
    ));
    assert!(matches!(
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("mock\0player")),
        Err(Error::InvalidCString(_))
    ));
    assert_eq!(PLAYER_CREATES.get(), 0);
    drop(
        crate::file::FilePlayer::new(
            &api,
            &FilePlayerOptions::new("mock player").groups([] as [&str; 0]),
        )
        .unwrap(),
    );
    assert_eq!(PLAYER_CREATES.get(), 1);
}

#[test]
fn a_family_the_library_lacks_is_refused_as_that_capability_by_the_entry_point_it_is_missing() {
    use crate::capabilities::CapabilityUnavailable;
    let api = FileApi::with_runtime(sys::FileRuntime::test_double(
        Err(c"NDIlib_file_player_seek_to_frame"),
        Ok(mock::recording()),
    ));
    assert_eq!(
        (api.status(FileFamily::Playback)).reason(),
        Some(CapabilityUnavailable::SymbolUnavailable)
    );
    assert!(!api.supports(FileFamily::Playback));
    assert!(api.supports(FileFamily::Recording));
    assert!(api.require_family(FileFamily::Recording).is_ok());
    assert!(matches!(
        api.require_family(FileFamily::Playback),
        Err(Error::UnsupportedCapability {
            capability: crate::capabilities::Capability::File(FileFamily::Playback),
            refusal: crate::CapabilityRefusal::SymbolUnavailable(missing),
        }) if missing == c"NDIlib_file_player_seek_to_frame"
    ));
    // Refused before anything is checked or called, even for a path that
    // would be refused too.
    let async_probe_error = |path| {
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        let mut probe = AsyncFilePlayer::probe(&api, path).submit();
        let Poll::Ready(result) =
            Pin::new(&mut probe).poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("a missing playback family queued probe work");
        };
        result.unwrap_err()
    };
    for error in [
        crate::file::FilePlayer::probe(&api, "recording.mkv").unwrap_err(),
        crate::file::FilePlayer::probe(&api, "").unwrap_err(),
        crate::file::FilePlayer::probe(&api, "bad\0path.mkv").unwrap_err(),
        async_probe_error("recording.mkv"),
        async_probe_error(""),
        async_probe_error("bad\0path.mkv"),
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("mock player")).unwrap_err(),
        crate::file::FilePlayer::new(&api, &FilePlayerOptions::new("")).unwrap_err(),
    ] {
        let Error::UnsupportedCapability {
            capability: crate::capabilities::Capability::File(family),
            refusal: crate::CapabilityRefusal::SymbolUnavailable(missing),
        } = error
        else {
            panic!("expected the playback family to be unavailable: {error}");
        };
        assert_eq!(family, FileFamily::Playback);
        assert_eq!(missing, c"NDIlib_file_player_seek_to_frame");
    }
    // The family the library provides is unaffected.
    let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap());
    assert!(!matches!(
        crate::file::FileRecorder::new(&api, &options),
        Err(Error::UnsupportedCapability { .. })
    ));
}

/// Take every queued event, so the session's state follows what the device did.
fn take_events(recorder: &mut FileRecorder) {
    while recorder.next_event(Duration::ZERO).unwrap().is_some() {}
}

#[test]
fn a_session_the_sdk_ended_itself_can_be_followed_by_another() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.end_session_itself();
    take_events(&mut recorder);
    assert_eq!(recorder.state(), RecorderState::Closed);
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyClosed);
    // The library starts again only once it has been told to stop, although
    // the session it would stop is already over.
    assert_eq!(
        recorder
            .start(&target("second"), RecordingTimecode::Preserve)
            .unwrap(),
        2
    );
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
}

#[test]
fn finishing_a_session_the_sdk_is_ending_itself_still_tells_it_to_stop() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.end_session_itself();
    // Up to the error event: the session is stopping, by the library's choice.
    for _ in 0..3 {
        recorder.next_event(Duration::ZERO).unwrap().unwrap();
    }
    assert_eq!(recorder.state(), RecorderState::Stopping);
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyStopping);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
    recorder
        .start(&target("second"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
}

#[test]
fn a_malformed_stale_event_is_consumed_before_the_native_start() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
    .unwrap();
    device.push(mock::OPEN_FAILED, "old.mkv");
    device.oversize_next_event.store(true, Ordering::Release);
    assert_eq!(
        recorder
            .start(&target("malformed-stale"), RecordingTimecode::Preserve)
            .unwrap(),
        1
    );
    assert_eq!(device.starts.load(Ordering::Acquire), 1);
    assert_eq!(recorder.report().error_events(), 0);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn refused_starts_name_the_actual_timecode_entry_point() {
    for (timecode, expected) in [
        (RecordingTimecode::Preserve, c"NDIlib_file_recorder_start"),
        (
            RecordingTimecode::GenerateFrom(
                SessionTimecode::new(crate::Timecode::from_ticks(123)).unwrap(),
            ),
            c"NDIlib_file_recorder_start_with_timecode",
        ),
    ] {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
        )
        .unwrap();
        device.fail_next_start.store(true, Ordering::Release);
        assert!(matches!(recorder.start(&target("unopenable"), timecode),
            Err(Error::NativeOperationFailed { operation }) if operation == expected));
    }
}

#[test]
fn a_refused_start_is_not_counted_in_any_session() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
    .unwrap();
    let refuse_start = |recorder: &mut FileRecorder| {
        device.fail_next_start.store(true, Ordering::Release);
        assert!(matches!(
            recorder.start(&target("unopenable"), RecordingTimecode::Preserve),
            Err(Error::NativeOperationFailed { .. })
        ));
        // The library's reason is still there to be read.
        let event = recorder.next_event(Duration::ZERO).unwrap().unwrap();
        assert_eq!(event.kind(), RecorderEventKind::OpenFailed);
    };

    // Before any session there is no report to count it in.
    refuse_start(&mut recorder);
    assert_eq!(recorder.state(), RecorderState::Idle);
    assert_eq!(recorder.report().error_events(), 0);

    // After one, the finished session's report stays as it was.
    recorder
        .start(&target("first"), RecordingTimecode::Preserve)
        .unwrap();
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
    refuse_start(&mut recorder);
    let report = recorder.report();
    assert_eq!(report.session(), 1);
    assert_eq!(report.error_events(), 0);
    assert!(report.closure_confirmed());
    assert_eq!(recorder.state(), RecorderState::Closed);
}

#[test]
fn the_source_can_be_changed_after_a_session_that_ended_unconfirmed() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.dropped_events.store(1, Ordering::Release);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Indeterminate(_)
    ));
    // Source selection is still allowed, although another start requires reset.
    recorder
        .set_source(&SourceSpec::new("another source").unwrap())
        .unwrap();
    assert_eq!(
        recorder.last_accepted_source().unwrap().name(),
        Some("another source")
    );
    recorder.abandon();
}

#[test]
fn waiting_on_a_final_report_preserves_queued_out_of_session_events() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::Closed(_)
    ));
    device.push(mock::OPEN_FAILED, "later.mkv");
    let before = device.captures.load(Ordering::Acquire);
    assert!(matches!(
        recorder.wait_for_finish(Duration::ZERO).unwrap(),
        Some(FinishStatus::Closed(_))
    ));
    assert_eq!(device.captures.load(Ordering::Acquire), before);
    assert_eq!(
        recorder.next_event(Duration::ZERO).unwrap().unwrap().kind(),
        RecorderEventKind::OpenFailed
    );
}

#[test]
fn finishing_without_a_wait_reports_a_close_the_stop_already_queued() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    let outcome = recorder.finish(Duration::ZERO).unwrap();
    let FinishStatus::Closed(report) = outcome else {
        panic!("the close was queued before the wait began: {outcome:?}");
    };
    assert!(report.closure_confirmed());
    assert!(!report.stopped_by_sdk());
}

#[test]
fn a_wait_that_ends_during_a_slow_stop_still_takes_the_queued_close() {
    let device = mock::Recorder::new(Closing::DuringSlowStop(Duration::from_millis(80)));
    let mut recorder = recording_on(&device);
    // As the async recorder waits: from before the stop, which outlasts the
    // wait.
    let deadline = Deadline::after(Duration::from_millis(10)).unwrap();
    recorder.request_stop();
    assert!(deadline.is_expired());
    let outcome = recorder.wait_until(deadline).unwrap();
    assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
}

#[test]
fn finish_starts_its_wait_once_the_stop_has_returned() {
    // The stop alone outlasts the wait, and the close comes after it: only a
    // wait that starts after the stop sees it.
    let device = mock::Recorder::new(Closing::AfterSlowStop(
        Duration::from_millis(300),
        Duration::from_millis(50),
    ));
    let mut recorder = recording_on(&device);
    let outcome = recorder.finish(Duration::from_millis(250)).unwrap();
    assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
}

#[test]
fn drop_spends_its_cleanup_budget_on_the_native_stop_before_waiting_for_closure() {
    let device = mock::Recorder::new(Closing::AfterSlowStop(
        Duration::from_millis(300),
        Duration::from_millis(50),
    ));
    let recorder = recording_with(
        &device,
        FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
            .drop_wait(Duration::from_millis(250)),
    );
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    assert!(!device.closed_before_destroy.load(Ordering::Acquire));
}

#[test]
fn a_timeout_out_of_range_is_refused_before_the_stop() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    let too_long = crate::MAX_TIMEOUT + Duration::from_nanos(1);
    for result in [
        recorder.finish(too_long).map(|_| ()),
        recorder.wait_for_finish(too_long).map(|_| ()),
    ] {
        assert!(
            matches!(&result, Err(Error::InvalidInput(crate::InputDefect::TimeoutTooLong { requested })) if *requested == too_long),
            "{result:?}"
        );
    }
    assert_eq!(device.stops.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert!(matches!(
        recorder.finish(crate::MAX_TIMEOUT).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn injected_metadata_reaches_the_sdk_as_the_frames_own_text() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    let metadata = crate::MetadataFrame::new("<marker cue=\"intro\"/>")
        .unwrap()
        .with_timecode(crate::Timecode::from_ticks(1_234));
    recorder.inject_metadata(&metadata).unwrap();
    let injected = device.injected.lock().unwrap();
    let [frame] = injected.as_slice() else {
        panic!("one frame was injected, not {}", injected.len());
    };
    // The frame's own storage, not a copy made for the call.
    assert_eq!(frame.address, metadata.data().as_ptr() as usize);
    assert_eq!(
        usize::try_from(frame.length).unwrap(),
        metadata.data().len() + 1
    );
    assert_eq!(frame.bytes, b"<marker cue=\"intro\"/>\0");
    assert_eq!(frame.timecode, 1_234);
}

#[test]
fn a_session_the_sdk_ended_is_reported_as_stopped_by_it() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.end_session_itself();
    take_events(&mut recorder);
    let report = recorder.report();
    assert_eq!(recorder.state(), RecorderState::Closed);
    assert!(report.closure_confirmed());
    assert!(report.stopped_by_sdk());

    // A stop requested once the SDK has stopped recording, before its events
    // were taken, does not make the caller the one who ended the session.
    recorder
        .start(&target("second"), RecordingTimecode::Preserve)
        .unwrap();
    take_events(&mut recorder);
    assert!(!recorder.report().stopped_by_sdk(), "an open session");
    device.end_session_itself();
    let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(1)).unwrap() else {
        panic!("the session was closed");
    };
    assert!(report.stopped_by_sdk());

    // A session stopped on request was not.
    recorder
        .start(&target("third"), RecordingTimecode::Preserve)
        .unwrap();
    let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(1)).unwrap() else {
        panic!("the session was closed");
    };
    assert!(!report.stopped_by_sdk());
}

/// A directory of its own for one test's targets, removed when it ends.
struct Directory(std::path::PathBuf);

impl Directory {
    fn new(test: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "grafton-ndi-mock-{test}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn target(&self, name: &str) -> RecordingTarget {
        RecordingTarget::new(self.0.join(name)).unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn exact() -> FileRecorderOptions {
    FileRecorderOptions::new(SourceSpec::new("mock source").unwrap()).naming(FileNaming::Exact)
}

fn io_kind<T: std::fmt::Debug>(result: Result<T>) -> std::io::ErrorKind {
    match result {
        Err(Error::Io(error)) => error.kind(),
        other => panic!("expected a filesystem error: {other:?}"),
    }
}

#[test]
fn unreportable_recording_paths_never_reach_native_start_or_rollover() {
    let directory = Directory::new("path-limit");
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &exact()).unwrap();
    let base = directory.0.to_str().unwrap();
    let path_bytes = sys::bindings::NDIlib_file_recorder_event_path_max_size as usize - 1;
    let too_long = RecordingTarget::new(format!(
        "{base}/{}",
        "x".repeat(path_bytes - base.len() - 1)
    ))
    .unwrap();
    assert!(matches!(
        recorder.start(&too_long, RecordingTimecode::Preserve),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(device.starts.load(Ordering::Acquire), 0);
    recorder
        .start(&directory.target("take.mkv"), RecordingTimecode::Preserve)
        .unwrap();
    assert!(matches!(
        recorder.rollover(&too_long),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(device.starts.load(Ordering::Acquire), 1);
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn conflicting_options_are_refused_before_the_sdk_creates_anything() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let indexed = || FileRecorderOptions::new(SourceSpec::new("mock source").unwrap());
    let requested = crate::MAX_TIMEOUT + Duration::from_millis(1);
    for (options, expected) in [
        (
            exact().format_change_rollover(true),
            crate::InputDefect::ExactNameFormatRollover,
        ),
        (
            exact().rollover_size(4096),
            crate::InputDefect::ExactNameSizeRollover,
        ),
        (
            indexed().tracks(MediaTypes::NONE),
            crate::InputDefect::EmptyRecordingTracks,
        ),
        (
            indexed().drop_wait(requested),
            crate::InputDefect::TimeoutTooLong {
                requested: requested.into(),
            },
        ),
        (
            indexed().naming(FileNaming::Pattern("{prefix}_{take}.mkv".into())),
            crate::InputDefect::FilenamePatternToken {
                token: "take".into(),
            },
        ),
    ] {
        let Err(Error::InvalidInput(cause)) =
            crate::file::FileRecorder::new(&mock::recorder_api(), &options)
        else {
            panic!("conflicting options were not refused");
        };
        assert_eq!(cause, expected);
    }
    assert_eq!(device.creates.load(Ordering::Acquire), 0);

    // What creation tells the SDK.
    let created = |options: FileRecorderOptions| {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        drop(crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap());
        (
            device
                .created_without_format_rollover
                .load(Ordering::Acquire),
            device.created_rollover_bytes.load(Ordering::Acquire),
        )
    };
    assert_eq!(created(indexed()), (false, 0));
    assert_eq!(created(indexed().rollover_size(4096)), (false, 4096));
    assert_eq!(created(indexed().format_change_rollover(false)), (true, 0));
    assert_eq!(created(exact()), (true, 0));
    assert_eq!(created(exact().format_change_rollover(false)), (true, 0));
}

#[test]
fn the_naming_and_the_proxy_reach_the_sdk_as_its_settings() {
    let created = |options: FileRecorderOptions| {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        drop(crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap());
        let pattern = device.created_pattern.lock().unwrap().clone();
        (
            pattern,
            device.created_without_proxy.load(Ordering::Acquire),
        )
    };
    let options = || FileRecorderOptions::new(SourceSpec::new("mock source").unwrap());
    // Indexed names leave the pattern to the SDK's own default.
    assert_eq!(created(options()), (None, false));
    assert_eq!(
        created(options().naming(FileNaming::Exact).proxy(false)),
        (Some("{prefix}".to_owned()), true)
    );
    assert_eq!(
        created(options().naming(FileNaming::Pattern("{prefix}/{index:03}.mkv".into()))),
        (Some("{prefix}/{index:03}.mkv".to_owned()), false)
    );
}

#[test]
fn a_reset_to_conflicting_options_keeps_the_recorder() {
    let old = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&old);
    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.install();
    assert!(matches!(
        recorder.reset(&exact().rollover_size(4096)),
        Err(Error::InvalidInput(
            crate::InputDefect::ExactNameSizeRollover
        ))
    ));
    assert_eq!(replacement.creates.load(Ordering::Acquire), 0);
    assert_eq!(old.destroys.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn an_exact_recorder_changes_source_only_between_sessions() {
    let directory = Directory::new("exact-source");
    let device = mock::Recorder::new(Closing::Never);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &exact()).unwrap();
    let another = || SourceSpec::new("another source").unwrap();

    recorder.set_source(&another()).unwrap();
    assert_eq!(device.source_changes.load(Ordering::Acquire), 1);

    recorder
        .start(&directory.target("take.mkv"), RecordingTimecode::Preserve)
        .unwrap();
    assert!(matches!(
        recorder.set_source(&another()),
        Err(Error::InvalidState(StateConflict::ExactFileName))
    ));
    assert_eq!(recorder.state(), RecorderState::Recording);
    recorder.request_stop();
    assert!(matches!(
        recorder.set_source(&another()),
        Err(Error::InvalidState(StateConflict::ExactFileName))
    ));
    assert_eq!(device.source_changes.load(Ordering::Acquire), 1);
    assert_eq!(
        recorder.last_accepted_source().unwrap().name(),
        Some("another source")
    );
    // The stand-in never reports the close; do not wait for it.
    recorder.abandon();
}

#[test]
fn an_indexed_recorder_refuses_a_change_of_source_only_while_stopping() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    recorder
        .set_source(&SourceSpec::new("another source").unwrap())
        .unwrap();
    recorder.request_stop();
    assert!(matches!(
        recorder.set_source(&SourceSpec::new("a third source").unwrap()),
        Err(Error::InvalidState(StateConflict::Stopping))
    ));
    assert_eq!(device.source_changes.load(Ordering::Acquire), 1);
    recorder.abandon();
}

#[test]
fn an_exact_target_that_exists_is_refused_before_the_sdk_sees_it() {
    let directory = Directory::new("exact-exists");
    std::fs::write(directory.0.join("taken.mkv"), b"kept").unwrap();
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &exact()).unwrap();

    assert_eq!(
        io_kind(recorder.start(&directory.target("taken.mkv"), RecordingTimecode::Preserve)),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(device.starts.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Idle);

    recorder
        .start(&directory.target("first.mkv"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(
        io_kind(recorder.rollover(&directory.target("taken.mkv"))),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    recorder.rollover(&directory.target("second.mkv")).unwrap();
    assert_eq!(device.rollovers.load(Ordering::Acquire), 1);
    assert_eq!(
        std::fs::read(directory.0.join("taken.mkv")).unwrap(),
        b"kept"
    );
}

#[test]
fn the_directory_a_pattern_names_is_checked_before_the_sdk_sees_it() {
    let directory = Directory::new("pattern-directory");
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap())
        .naming(FileNaming::Pattern("{prefix}/{index:03}.mkv".into()));
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap();

    // The target's own directory exists; the one the pattern names does not.
    assert_eq!(
        io_kind(recorder.start(&directory.target("first"), RecordingTimecode::Preserve)),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(device.starts.load(Ordering::Acquire), 0);
    std::fs::create_dir(directory.0.join("first")).unwrap();
    recorder
        .start(&directory.target("first"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(
        io_kind(recorder.rollover(&directory.target("missing"))),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
}

/// Exercise the public start and rollover paths against a permission-restricted
/// child directory, keeping the active session outside that directory.
#[cfg(any(unix, windows))]
fn unwritable_directory_is_refused_before_native_calls(directory: &Directory) {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = FileRecorder::new(&mock::recorder_api(), &exact()).unwrap();
    let denied = directory.target("denied/take.mkv");
    let error = recorder
        .start(&denied, RecordingTimecode::Preserve)
        .unwrap_err();
    assert!(error.to_string().contains("cannot be written to"));
    assert_eq!(
        io_kind::<()>(Err(error)),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(device.starts.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Idle);

    recorder
        .start(&directory.target("live.mkv"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(
        io_kind(recorder.rollover(&denied)),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert_eq!(
        std::fs::read_dir(directory.0.join("denied"))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn unwritable_unix_recording_directories_never_reach_the_sdk() {
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let directory = Directory::new("unwritable-unix");
    let denied = directory.0.join("denied");
    std::fs::create_dir(&denied).unwrap();
    let _restore = Restore(denied.clone());
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o555)).unwrap();
    let path = std::ffi::CString::new(denied.as_os_str().as_bytes()).unwrap();
    // Match the SDK's effective identity, including when it can override mode
    // bits. Android applications have equal real and effective IDs.
    // SAFETY: `path` retains a NUL-terminated directory path for either query.
    #[cfg(not(target_os = "android"))]
    let access = unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    };
    #[cfg(target_os = "android")]
    let access = unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) };
    if access == 0 {
        crate::test_support::unsupported("this process can override directory write permissions");
        return;
    }
    unwritable_directory_is_refused_before_native_calls(&directory);
}

#[cfg(target_os = "linux")]
#[test]
fn an_effective_identity_without_write_access_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    const CHILD_DIRECTORY: &str = "GRAFTON_NDI_PERMISSION_CHILD_DIRECTORY";
    const IDENTITY_UNAVAILABLE: i32 = 77;
    if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
        // This isolated subprocess alone changes identity. The parent retains
        // ownership of the fixture and removes it after this process exits.
        let directory = std::mem::ManuallyDrop::new(Directory(directory.into()));
        // SAFETY: geteuid/getuid take no arguments and inspect this process.
        assert_eq!(unsafe { libc::getuid() }, 0);
        assert_eq!(unsafe { libc::geteuid() }, 0);
        // SAFETY: only this test's subprocess is affected. Its real UID stays
        // root, so access(2) would incorrectly allow the denied directory.
        if unsafe { libc::seteuid(65534) } != 0 {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EPERM | libc::EINVAL)) {
                // User namespaces may map root alone, or forbid changing
                // identity. Let the parent report that limitation visibly.
                std::process::exit(IDENTITY_UNAVAILABLE);
            }
            panic!("cannot set the subprocess's effective identity: {error}");
        }
        unwritable_directory_is_refused_before_native_calls(&directory);
        return;
    }
    // SAFETY: geteuid/getuid take no arguments and inspect this process.
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
        crate::test_support::unsupported("this identity-switching regression requires root");
        return;
    }
    let directory = Directory::new("effective-identity");
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    let denied = directory.0.join("denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "file::lifecycle_tests::an_effective_identity_without_write_access_is_refused",
            "--nocapture",
        ])
        .env(CHILD_DIRECTORY, &directory.0)
        .output()
        .unwrap();
    if result.status.code() == Some(IDENTITY_UNAVAILABLE) {
        crate::test_support::unsupported(
            "this process cannot switch to an unprivileged effective identity",
        );
        return;
    }
    assert!(
        result.status.success(),
        "effective-identity child failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}

#[cfg(windows)]
#[test]
fn acl_denied_windows_recording_directories_never_reach_the_sdk() {
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let restored = std::process::Command::new("icacls")
                .arg(&self.0)
                .args(["/remove:d", "*S-1-1-0", "/q"])
                .output()
                .unwrap();
            assert!(
                restored.status.success(),
                "{}",
                String::from_utf8_lossy(&restored.stderr)
            );
        }
    }
    let directory = Directory::new("unwritable-windows");
    let denied = directory.0.join("denied");
    std::fs::create_dir(&denied).unwrap();
    let _restore = Restore(denied.clone());
    // Deny Everyone's write-data/add-file right on this test-owned directory.
    // A SID avoids dependence on the runner's account name or Windows language.
    let restricted = std::process::Command::new("icacls")
        .arg(&denied)
        .args(["/deny", "*S-1-1-0:(WD)", "/q"])
        .output()
        .unwrap();
    assert!(
        restricted.status.success(),
        "{}",
        String::from_utf8_lossy(&restricted.stderr)
    );
    unwritable_directory_is_refused_before_native_calls(&directory);
}

#[cfg(windows)]
#[test]
fn low_integrity_recording_honours_the_directory_label() {
    use std::{
        mem::size_of,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSidToSidA, DuplicateTokenEx, GetLengthSid,
            ImpersonateLoggedOnUser, RevertToSelf, SecurityImpersonation, SetTokenInformation,
            TokenImpersonation, TokenIntegrityLevel, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT,
            TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    // winnt.h's integrity-group attribute; windows-sys exposes it only with
    // SystemServices, which the production library does not otherwise need.
    const SE_GROUP_INTEGRITY: u32 = 0x20;

    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // SAFETY: this guard is confined to the thread impersonated below.
            assert_ne!(unsafe { RevertToSelf() }, 0);
        }
    }
    struct Sid(windows_sys::Win32::Security::PSID);
    impl Drop for Sid {
        fn drop(&mut self) {
            // SAFETY: ConvertStringSidToSidA returned this allocation once.
            unsafe { LocalFree(self.0) };
        }
    }
    fn succeeded(value: i32) {
        assert_ne!(value, 0, "{}", std::io::Error::last_os_error());
    }

    let directory = Directory::new("integrity-windows");
    for (name, label) in [("low", "(OI)(CI)L"), ("medium", "(OI)(CI)M")] {
        let child = directory.0.join(name);
        std::fs::create_dir(&child).unwrap();
        let output = std::process::Command::new("icacls")
            .arg(&child)
            .args(["/setintegritylevel", label, "/q"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "icacls: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = FileRecorder::new(&mock::recorder_api(), &exact()).unwrap();
    let low = directory.target("low/take.mkv");
    let medium = directory.target("medium/take.mkv");

    let mut process_token = ptr::null_mut();
    // SAFETY: the process pseudo-handle is live and the out handle is writable.
    succeeded(unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut process_token,
        )
    });
    // SAFETY: the successful open returned one owned token handle.
    let process_token = unsafe { OwnedHandle::from_raw_handle(process_token) };
    let mut token = ptr::null_mut();
    // SAFETY: duplicate only this process's identity; every input stays live.
    succeeded(unsafe {
        DuplicateTokenEx(
            process_token.as_raw_handle(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_ADJUST_DEFAULT,
            ptr::null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut token,
        )
    });
    // SAFETY: DuplicateTokenEx returned one distinct owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut sid = ptr::null_mut();
    // SAFETY: the constant SID is terminated and the output is writable.
    succeeded(unsafe { ConvertStringSidToSidA(c"S-1-16-4096".as_ptr().cast(), &mut sid) });
    let sid = Sid(sid);
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    // SAFETY: the SID came from a successful conversion and stays live.
    let label_bytes = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(sid.0) };
    // SAFETY: this writable duplicate has TOKEN_ADJUST_DEFAULT; label and its
    // referenced SID remain live for the call, and its integrity is only lowered.
    succeeded(unsafe {
        SetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
            label_bytes,
        )
    });
    // SAFETY: the duplicate is an impersonation token for this process's user.
    succeeded(unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) });
    let revert = Revert;

    // The real kernel evaluates the same DACL with two different labels.
    // Only the low-labelled directory permits creation by the lowered token.
    assert_eq!(
        io_kind(recorder.start(&medium, RecordingTimecode::Preserve)),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(device.starts.load(Ordering::Acquire), 0);
    recorder.start(&low, RecordingTimecode::Preserve).unwrap();
    assert_eq!(device.starts.load(Ordering::Acquire), 1);
    assert_eq!(
        io_kind(recorder.rollover(&medium)),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    drop(revert);
}

#[test]
fn values_a_refused_call_was_given_stay_with_the_caller_to_pass_again() {
    let directory = Directory::new("borrowed");
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let options = FileRecorderOptions::new(SourceSpec::new("mock source").unwrap());
    let mut recorder = crate::file::FileRecorder::new(&mock::recorder_api(), &options).unwrap();

    // The indexed pattern puts each file in the directory the target names.
    let first = directory.target("first/take");
    assert_eq!(
        io_kind(recorder.start(&first, RecordingTimecode::Preserve)),
        std::io::ErrorKind::NotFound
    );
    std::fs::create_dir(directory.0.join("first")).unwrap();
    recorder.start(&first, RecordingTimecode::Preserve).unwrap();

    let second = directory.target("second/take");
    assert_eq!(
        io_kind(recorder.rollover(&second)),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(recorder.state(), RecorderState::Recording);
    std::fs::create_dir(directory.0.join("second")).unwrap();
    recorder.rollover(&second).unwrap();
    assert_eq!(device.rollovers.load(Ordering::Acquire), 1);

    let source = SourceSpec::new("another source").unwrap();
    recorder.set_source(&source).unwrap();
    assert_eq!(recorder.last_accepted_source(), Some(&source));
    assert!(matches!(
        recorder.finish(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));

    // A reset the SDK refuses leaves the options to try again with.
    let replacement = mock::Recorder::new(Closing::AtStop);
    replacement.refuse_creation.store(true, Ordering::Release);
    replacement.install();
    assert!(matches!(
        recorder.reset(&options),
        Err(Error::NativeOperationFailed {
            operation}) if operation.name() == c"NDIlib_file_recorder_create"
    ));
    replacement.refuse_creation.store(false, Ordering::Release);
    replacement.install();
    recorder.reset(&options).unwrap();
    assert_eq!(replacement.creates.load(Ordering::Acquire), 1);
    assert_eq!(recorder.last_accepted_source(), Some(&options.source));
    assert_eq!(options.source, SourceSpec::new("mock source").unwrap());
}

#[test]
fn players_borrow_their_options() {
    let api = FileApi::with_tables(
        sys::FilePlayback {
            NDIlib_file_player_create: create_player,
            ..mock::playback()
        },
        mock::recording(),
    );
    let options = FilePlayerOptions::new("mock player");
    let first = crate::file::FilePlayer::new(&api, &options).unwrap();
    let second = crate::file::FilePlayer::new(&api, &options).unwrap();
    assert!(format!("{first:?}").contains("mock player"), "{first:?}");
    assert!(format!("{second:?}").contains("mock player"), "{second:?}");
    assert_eq!(options.name, "mock player");
}

#[test]
fn selected_endpoints_and_deliberate_names_cross_creation_and_source_change() {
    use crate::{Source, SourceAddress};
    let selected = Source::new("STUDIO (Camera 2)")
        .unwrap()
        .with_address(SourceAddress::new("192.0.2.10:5962").unwrap());
    let spec = SourceSpec::try_from(&selected).unwrap();
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = crate::file::FileRecorder::new(
        &mock::recorder_api(),
        &FileRecorderOptions::new(spec.clone()),
    )
    .unwrap();
    assert_eq!(recorder.last_accepted_source(), Some(&spec));
    let second = Source::new("STUDIO (Program)")
        .unwrap()
        .with_address(SourceAddress::new("ndi://camera.local:5963/path?opaque=1").unwrap());
    recorder
        .set_source(&SourceSpec::try_from(&second).unwrap())
        .unwrap();
    recorder
        .set_source(&SourceSpec::new(selected.name()).unwrap())
        .unwrap();
    assert_eq!(
        *device.selected_sources.lock().unwrap(),
        vec![
            (None, Some("192.0.2.10:5962".into())),
            (None, Some("ndi://camera.local:5963/path?opaque=1".into())),
            (Some("STUDIO (Camera 2)".into()), None),
        ]
    );
    let before = recorder.last_accepted_source().unwrap().clone();
    assert!(SourceSpec::try_from(
        &Source::new("valid name")
            .unwrap()
            .with_address(SourceAddress::new("").unwrap())
    )
    .is_err());
    assert_eq!(recorder.last_accepted_source(), Some(&before));
    assert_eq!(device.source_changes.load(Ordering::Acquire), 2);
}
