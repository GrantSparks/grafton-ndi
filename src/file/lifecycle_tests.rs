//! The recorder's and player's lifecycle against stand-in SDK entry points.

use super::{
    mock::{self, Closing, FILE_CLOSED, ROLLOVER, TAKE},
    *,
};
use crate::sys;
use std::{
    cell::Cell,
    sync::{atomic::Ordering, Arc},
    time::Instant,
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
    let mut recorder = mock::recorder_api().create_recorder(options).unwrap();
    recorder
        .start(target("first"), RecordingTimecode::Preserve)
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
    recorder.rollover(target("second")).unwrap();
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
        .set_source(SourceSpec::new("another source").unwrap())
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
    recorder.rollover(target("second")).unwrap();
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
        .start(target("again"), RecordingTimecode::Preserve)
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
    assert!(matches!(error, FileError::Protocol(_)), "{error}");
    let outcome = recorder.finish(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(outcome, FinishStatus::Indeterminate(_)),
        "{outcome:?}"
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
        .with_drop_wait(Duration::from_millis(60))
        .unwrap();
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
        .create_recorder(RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    assert_eq!(recorder.request_stop(), StopRequest::NotStarted);
    assert!(matches!(
        recorder.finish(Duration::ZERO),
        Err(FileError::InvalidState(_))
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
        .create_recorder(RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    let missing = || {
        RecordingTarget::new(std::env::temp_dir().join("grafton-ndi-no-such-directory/take"))
            .unwrap()
    };
    let not_found = |result: FileResult<()>| match result {
        Err(FileError::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected the directory to be reported missing: {other:?}"),
    };

    not_found(
        recorder
            .start(missing(), RecordingTimecode::Preserve)
            .map(drop),
    );
    assert_eq!(recorder.state(), RecorderState::Idle);

    recorder
        .start(target("first"), RecordingTimecode::Preserve)
        .unwrap();
    // The SDK would accept this rollover and then end the session.
    not_found(recorder.rollover(missing()));
    assert_eq!(device.rollovers.load(Ordering::Acquire), 0);
    assert_eq!(recorder.state(), RecorderState::Recording);
    recorder.rollover(target("second")).unwrap();
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
    let options = RecorderOptions::new(SourceSpec::new("mock source").unwrap())
        .with_tracks(TrackSelection::METADATA);
    let mut recorder = recording_with(&device, options);
    let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(1)).unwrap() else {
        panic!("the file was reported closed");
    };
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.empty_files(), 0);
}

thread_local! {
    static PLAYER_STOPS: Cell<usize> = const { Cell::new(0) };
    static PLAYER_DESTROYS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn create_player(
    _: *const sys::bindings::NDIlib_file_player_create_t,
) -> sys::bindings::NDIlib_file_player_instance_t {
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
        .create_player(PlayerOptions::new("mock player").unwrap())
        .unwrap();
    drop(player);
    assert_eq!(PLAYER_STOPS.get(), 1);
    assert_eq!(PLAYER_DESTROYS.get(), 1);
}

#[test]
fn a_family_the_library_lacks_is_reported_by_the_entry_point_it_is_missing() {
    let api = FileApi {
        playback: Err(c"NDIlib_file_player_seek_to_frame"),
        ..mock::recorder_api()
    };
    for error in [
        api.probe("recording.mkv").unwrap_err(),
        api.create_player(PlayerOptions::new("mock player").unwrap())
            .unwrap_err(),
    ] {
        let FileError::Unavailable(reason) = error else {
            panic!("expected the playback family to be unavailable: {error}");
        };
        assert!(
            reason.contains("NDIlib_file_player_seek_to_frame"),
            "{reason}"
        );
    }
}

#[test]
fn waits_are_rounded_up_and_the_largest_is_refused() {
    assert_eq!(timeout_ms(Duration::from_nanos(1)).unwrap(), 1);
    assert_eq!(timeout_ms(Duration::ZERO).unwrap(), 0);
    assert_eq!(
        timeout_ms(Duration::from_millis(u64::from(u32::MAX) - 1)).unwrap(),
        u32::MAX - 1
    );
    assert!(timeout_ms(Duration::from_millis(u32::MAX.into())).is_err());
    assert!(timeout_ms(Duration::MAX).is_err());
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
            .start(target("second"), RecordingTimecode::Preserve)
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
        .start(target("second"), RecordingTimecode::Preserve)
        .unwrap();
    assert_eq!(device.stops.load(Ordering::Acquire), 1);
}

#[test]
fn a_refused_start_is_not_counted_in_any_session() {
    let device = mock::Recorder::new(Closing::AtStop);
    device.install();
    let mut recorder = mock::recorder_api()
        .create_recorder(RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    let refuse_start = |recorder: &mut FileRecorder| {
        device.fail_next_start.store(true, Ordering::Release);
        assert!(matches!(
            recorder.start(target("unopenable"), RecordingTimecode::Preserve),
            Err(FileError::Rejected(_))
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
        .start(target("first"), RecordingTimecode::Preserve)
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
        .set_source(SourceSpec::new("another source").unwrap())
        .unwrap();
    assert_eq!(
        recorder.last_accepted_source().unwrap().name(),
        Some("another source")
    );
}
