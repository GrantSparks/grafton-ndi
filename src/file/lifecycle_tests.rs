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
        RecorderOptions::new(SourceSpec::new("mock source").unwrap()),
    )
}

fn recording_with(device: &Arc<mock::Recorder>, options: RecorderOptions) -> FileRecorder {
    device.install();
    let mut recorder = mock::recorder_api().create_recorder(&options).unwrap();
    recorder
        .start(&target("first"), RecordingTimecode::Preserve)
        .unwrap();
    recorder
}

fn target(name: &str) -> RecordingTarget {
    RecordingTarget::new(std::env::temp_dir().join(format!("mock-not-created-{name}"))).unwrap()
}

#[test]
fn a_timeout_keeps_the_recorder_and_a_later_close_completes_without_a_second_stop() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    assert!(matches!(
        recorder.wait_finished(Duration::from_millis(30)).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    assert_eq!(device.destroys.load(Ordering::Acquire), 0);
    device.push(FILE_CLOSED, TAKE);
    assert!(matches!(
        recorder.wait_finished(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
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
    while recorder.poll_event(Duration::ZERO).unwrap().is_some() {}
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert_eq!(
        recorder.reported_current_file().unwrap().as_bytes(),
        b"second.mkv"
    );
    recorder.request_stop();
    device.push(FILE_CLOSED, "second.mkv");
    let FinishStatus::Closed(report) = recorder.wait_finished(Duration::from_secs(1)).unwrap()
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
        .reset(&RecorderOptions::new(
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
        recorder.reset(&RecorderOptions::new(
            SourceSpec::new("replacement").unwrap()
        )),
        Err(Error::NativeOperationFailed {
            operation: "recorder creation"
        })
    ));
    assert_eq!(old.destroys.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Stopping);
    old.push(FILE_CLOSED, TAKE);
    assert!(matches!(
        recorder.wait_finished(Duration::from_secs(1)).unwrap(),
        FinishStatus::Closed(_)
    ));
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
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyEnded);
    // The session is over, so another can begin.
    device.dropped_events.store(1, Ordering::Release);
    recorder
        .start(&target("again"), RecordingTimecode::Preserve)
        .unwrap();
}

#[test]
fn a_malformed_close_event_still_closes_the_session() {
    let device = mock::Recorder::new(Closing::Never);
    let mut recorder = recording_on(&device);
    recorder.request_stop();
    let mut event = mock::recorder_event(FILE_CLOSED, TAKE);
    event.source_format_payload.video_track_count = 3;
    device.push_event(event);
    while recorder
        .poll_event(Duration::ZERO)
        .is_ok_and(|event| event.is_some())
    {}
    assert!(matches!(
        recorder.wait_finished(Duration::from_millis(50)).unwrap(),
        FinishStatus::Closed(_)
    ));
}

#[test]
fn an_event_larger_than_this_crate_expects_is_rejected_and_taints_the_session() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    device.oversize_next_event.store(true, Ordering::Release);
    let error = recorder.poll_event(Duration::ZERO).unwrap_err();
    assert!(matches!(error, Error::UnexpectedSdkResult(_)), "{error}");
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Indeterminate(_)),
        "{outcome:?}"
    );
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
    // The close comes well after the malformed event, which used to end the
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
    let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap())
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
    let mut recorder = mock::recorder_api()
        .create_recorder(&RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    assert_eq!(recorder.request_stop(), StopRequest::NotStarted);
    assert!(matches!(
        recorder.finish(Duration::ZERO),
        Err(Error::InvalidState(StateError::StopNotRequested))
    ));
    drop(recorder);
    assert_eq!(device.stops.load(Ordering::Acquire), 0);
    assert_eq!(device.destroys.load(Ordering::Acquire), 1);
}

#[test]
fn a_target_in_a_directory_that_does_not_exist_is_refused_before_the_sdk_sees_it() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = mock::recorder_api()
        .create_recorder(&RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
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
    let FinishStatus::Closed(report) = recorder.wait_finished(Duration::from_secs(1)).unwrap()
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
    let options =
        RecorderOptions::new(SourceSpec::new("mock source").unwrap()).tracks(MediaTypes::METADATA);
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
    let player = api
        .create_player(&PlayerOptions::new("mock player"))
        .unwrap();
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
        api.create_player(&PlayerOptions::new("")),
        Err(Error::InvalidInput(_))
    ));
    for options in [
        PlayerOptions::new("mock\0player"),
        PlayerOptions::new("mock player").groups("a\0b"),
    ] {
        assert!(matches!(
            api.create_player(&options),
            Err(Error::InvalidCString(_))
        ));
    }
    assert_eq!(PLAYER_CREATES.get(), 0);
    drop(
        api.create_player(&PlayerOptions::new("mock player").groups(""))
            .unwrap(),
    );
    assert_eq!(PLAYER_CREATES.get(), 1);
}

#[test]
fn a_family_the_library_lacks_is_refused_as_that_capability_by_the_entry_point_it_is_missing() {
    use crate::capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature};
    let api = FileApi::with_runtime(sys::FileRuntime::test_double(
        Err(c"NDIlib_file_player_seek_to_frame"),
        Ok(mock::recording()),
    ));
    assert_eq!(
        api.status(FileFamily::Playback),
        CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
    );
    assert!(!api.supports(FileFamily::Playback));
    assert!(api.supports(FileFamily::Recording));
    // Refused before anything is checked or called, even for a path that
    // would be refused too.
    for error in [
        api.probe("recording.mkv").unwrap_err(),
        api.probe("").unwrap_err(),
        api.create_player(&PlayerOptions::new("mock player"))
            .unwrap_err(),
        api.create_player(&PlayerOptions::new("")).unwrap_err(),
    ] {
        let Error::UnsupportedCapability {
            feature,
            reason,
            detail,
        } = error
        else {
            panic!("expected the playback family to be unavailable: {error}");
        };
        assert_eq!(feature, RuntimeFeature::File(FileFamily::Playback));
        assert_eq!(reason, CapabilityUnavailable::SymbolUnavailable);
        assert_eq!(detail, "NDIlib_file_player_seek_to_frame");
    }
    // The family the library provides is unaffected.
    let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap());
    assert!(!matches!(
        api.create_recorder(&options),
        Err(Error::UnsupportedCapability { .. })
    ));
}

/// Take every queued event, so the session's state follows what the device did.
fn take_events(recorder: &mut FileRecorder) {
    while recorder.poll_event(Duration::ZERO).unwrap().is_some() {}
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
        recorder.poll_event(Duration::ZERO).unwrap().unwrap();
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
fn a_refused_start_is_not_counted_in_any_session() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = mock::recorder_api()
        .create_recorder(&RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    let refuse_start = |recorder: &mut FileRecorder| {
        device.fail_next_start.store(true, Ordering::Release);
        assert!(matches!(
            recorder.start(&target("unopenable"), RecordingTimecode::Preserve),
            Err(Error::NativeOperationFailed { .. })
        ));
        // The library's reason is still there to be read.
        let event = recorder.poll_event(Duration::ZERO).unwrap().unwrap();
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
    // The session is over, as it is for `start`.
    recorder
        .set_source(&SourceSpec::new("another source").unwrap())
        .unwrap();
    assert_eq!(
        recorder.last_accepted_source().unwrap().name(),
        Some("another source")
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
    let outcome = recorder.wait_finished_from(deadline, |_| false).unwrap();
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
fn a_timeout_out_of_range_is_refused_before_the_stop() {
    let device = mock::Recorder::new(Closing::AtStop);
    let mut recorder = recording_on(&device);
    let too_long = crate::MAX_TIMEOUT + Duration::from_nanos(1);
    for result in [recorder.finish(too_long), recorder.wait_finished(too_long)] {
        assert!(
            matches!(&result, Err(Error::InvalidInput(reason)) if reason.contains("MAX_TIMEOUT")),
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

fn exact() -> RecorderOptions {
    RecorderOptions::new(SourceSpec::new("mock source").unwrap()).naming(FileNaming::Exact)
}

fn io_kind<T: std::fmt::Debug>(result: Result<T>) -> std::io::ErrorKind {
    match result {
        Err(Error::Io(error)) => error.kind(),
        other => panic!("expected a filesystem error: {other:?}"),
    }
}

#[test]
fn conflicting_options_are_refused_before_the_sdk_creates_anything() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let indexed = || RecorderOptions::new(SourceSpec::new("mock source").unwrap());
    for options in [
        exact().format_change_rollover(true),
        exact().rollover_size(4096),
        indexed().tracks(MediaTypes::NONE),
        indexed().drop_wait(crate::MAX_TIMEOUT + Duration::from_millis(1)),
        indexed().naming(FileNaming::Pattern("{prefix}_{take}.mkv".into())),
    ] {
        assert!(matches!(
            mock::recorder_api().create_recorder(&options),
            Err(Error::InvalidInput(_))
        ));
    }
    assert_eq!(device.creates.load(Ordering::Acquire), 0);

    // What creation tells the SDK.
    let created = |options: RecorderOptions| {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        drop(mock::recorder_api().create_recorder(&options).unwrap());
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
    let created = |options: RecorderOptions| {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        drop(mock::recorder_api().create_recorder(&options).unwrap());
        let pattern = device.created_pattern.lock().unwrap().clone();
        (
            pattern,
            device.created_without_proxy.load(Ordering::Acquire),
        )
    };
    let options = || RecorderOptions::new(SourceSpec::new("mock source").unwrap());
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
        Err(Error::InvalidInput(_))
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
    let mut recorder = mock::recorder_api().create_recorder(&exact()).unwrap();
    let another = || SourceSpec::new("another source").unwrap();

    recorder.set_source(&another()).unwrap();
    assert_eq!(device.source_changes.load(Ordering::Acquire), 1);

    recorder
        .start(&directory.target("take.mkv"), RecordingTimecode::Preserve)
        .unwrap();
    assert!(matches!(
        recorder.set_source(&another()),
        Err(Error::InvalidState(StateError::ExactFileName))
    ));
    assert_eq!(recorder.state(), RecorderState::Recording);
    recorder.request_stop();
    assert!(matches!(
        recorder.set_source(&another()),
        Err(Error::InvalidState(StateError::ExactFileName))
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
        Err(Error::InvalidState(StateError::Stopping))
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
    let mut recorder = mock::recorder_api().create_recorder(&exact()).unwrap();

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
    let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap())
        .naming(FileNaming::Pattern("{prefix}/{index:03}.mkv".into()));
    let mut recorder = mock::recorder_api().create_recorder(&options).unwrap();

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

#[test]
fn values_a_refused_call_was_given_stay_with_the_caller_to_pass_again() {
    let directory = Directory::new("borrowed");
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap());
    let mut recorder = mock::recorder_api().create_recorder(&options).unwrap();

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
            operation: "recorder creation"
        })
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
    let options = PlayerOptions::new("mock player");
    let first = api.create_player(&options).unwrap();
    let second = api.create_player(&options).unwrap();
    assert!(format!("{first:?}").contains("mock player"), "{first:?}");
    assert!(format!("{second:?}").contains("mock player"), "{second:?}");
    assert_eq!(options.name, "mock player");
}
