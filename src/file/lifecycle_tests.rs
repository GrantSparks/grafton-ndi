use super::*;
use std::{cell::RefCell, collections::VecDeque, ffi::c_void};

#[derive(Default)]
struct Mock {
    events: VecDeque<ffi::RecorderEvent>,
    stops: usize,
    destroys: usize,
    running: bool,
    dropped: u64,
    player_stops: usize,
    player_destroys: usize,
}
thread_local! { static MOCK: RefCell<Mock> = RefCell::new(Mock::default()); }
fn raw_event(kind: i32) -> ffi::RecorderEvent {
    let mut event = ffi::RecorderEvent {
        kind,
        ..Default::default()
    };
    for (dst, src) in event.path.iter_mut().zip(b"take.mkv") {
        *dst = *src as _;
    }
    event
}
unsafe extern "C" fn create_recorder(_: *const ffi::RecorderOptions) -> ffi::Handle {
    std::ptr::NonNull::<u8>::dangling()
        .as_ptr()
        .cast::<c_void>()
}
unsafe extern "C" fn start(_: ffi::Handle, _: *const std::ffi::c_char) -> bool {
    MOCK.with(|state| {
        let mut state = state.borrow_mut();
        state.running = true;
        state.events.push_back(raw_event(1));
    });
    true
}
unsafe extern "C" fn capture(_: ffi::Handle, out: *mut ffi::RecorderEvent, _: u32) -> i32 {
    MOCK.with(|state| state.borrow_mut().events.pop_front())
        .map_or(0, |event| {
            let kind = event.kind;
            unsafe { out.write(event) };
            kind
        })
}
unsafe extern "C" fn stop(_: ffi::Handle) {
    MOCK.with(|state| {
        let mut state = state.borrow_mut();
        state.stops += 1;
        state.running = false;
    });
}
unsafe extern "C" fn destroy(_: ffi::Handle) {
    MOCK.with(|state| state.borrow_mut().destroys += 1);
}
unsafe extern "C" fn running(_: ffi::Handle) -> bool {
    MOCK.with(|state| state.borrow().running)
}
unsafe extern "C" fn dropped(_: ffi::Handle) -> u64 {
    MOCK.with(|state| state.borrow().dropped)
}
unsafe extern "C" fn create_player(_: *const ffi::PlayerOptions) -> ffi::Handle {
    std::ptr::NonNull::<u8>::dangling().as_ptr().cast()
}
unsafe extern "C" fn player_stop(_: ffi::Handle) {
    MOCK.with(|state| state.borrow_mut().player_stops += 1);
}
unsafe extern "C" fn player_destroy(_: ffi::Handle) {
    MOCK.with(|state| state.borrow_mut().player_destroys += 1);
}
fn api() -> FileApi {
    MOCK.with(|state| *state.borrow_mut() = Mock::default());
    let mut functions = ffi::Functions::mocked();
    functions.recorder_create = create_recorder;
    functions.recorder_start = start;
    functions.recorder_capture = capture;
    functions.recorder_stop = stop;
    functions.recorder_destroy = destroy;
    functions.recorder_running = running;
    functions.recorder_dropped = dropped;
    functions.player_create = create_player;
    functions.player_stop = player_stop;
    functions.player_destroy = player_destroy;
    FileApi::mocked(functions)
}
fn recorder() -> FileRecorder {
    let mut recorder = api()
        .create_recorder(RecorderOptions::new(
            SourceSpec::new("mock source").unwrap(),
        ))
        .unwrap();
    recorder
        .start(
            RecordingTarget::new(std::env::temp_dir().join("mock-not-created")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    recorder
}

#[test]
fn timeout_retains_owner_and_later_close_completes_without_double_stop() {
    let mut recorder = recorder();
    recorder.poll_event(Duration::ZERO).unwrap();
    assert!(matches!(
        recorder.finish(Duration::ZERO).unwrap(),
        FinishStatus::TimedOut(_)
    ));
    MOCK.with(|state| {
        assert_eq!(state.borrow().destroys, 0);
        state.borrow_mut().events.push_back(raw_event(10));
    });
    assert!(matches!(
        recorder.wait_finished(Duration::from_millis(10)).unwrap(),
        FinishStatus::Closed(_)
    ));
    drop(recorder);
    MOCK.with(|state| {
        assert_eq!(state.borrow().stops, 1);
        assert_eq!(state.borrow().destroys, 1);
    });
}
#[test]
fn queued_started_event_is_not_mistaken_for_a_rollover_race() {
    let mut recorder = recorder();
    MOCK.with(|state| state.borrow_mut().events.push_back(raw_event(10)));
    assert!(matches!(
        recorder.finish(Duration::from_millis(10)).unwrap(),
        FinishStatus::Closed(_)
    ));
}
#[test]
fn drop_stops_and_destroys_once_without_waiting_for_a_missing_close_event() {
    let recorder = recorder();
    drop(recorder);
    MOCK.with(|state| {
        assert_eq!(state.borrow().stops, 1);
        assert_eq!(state.borrow().destroys, 1);
    });
}
#[test]
fn dropped_sdk_event_never_claims_finalization() {
    let mut recorder = recorder();
    recorder.poll_event(Duration::ZERO).unwrap();
    MOCK.with(|state| {
        let mut state = state.borrow_mut();
        state.dropped = 1;
        state.events.push_back(raw_event(10));
    });
    let outcome = recorder.finish(Duration::from_millis(10)).unwrap();
    assert!(matches!(outcome, FinishStatus::Indeterminate(_)));
    assert!(!recorder.report().closure_confirmed());
    assert_eq!(recorder.report().dropped_events(), 1);
}
#[test]
fn malformed_close_format_does_not_swallow_native_lifecycle_evidence() {
    let mut recorder = recorder();
    recorder.poll_event(Duration::ZERO).unwrap();
    recorder.request_stop();
    let mut event = raw_event(10);
    event.source_format_payload.video_track_count = 3;
    MOCK.with(|state| state.borrow_mut().events.push_back(event));
    assert!(matches!(
        recorder.poll_event(Duration::ZERO),
        Err(FileError::Protocol(_))
    ));
    assert!(matches!(
        recorder.wait_finished(Duration::from_millis(10)).unwrap(),
        FinishStatus::Closed(_)
    ));
}
#[test]
fn player_drop_pairs_stop_and_destroy() {
    let player = api()
        .create_player(PlayerOptions::new("mock player").unwrap())
        .unwrap();
    drop(player);
    MOCK.with(|state| {
        assert_eq!(state.borrow().player_stops, 1);
        assert_eq!(state.borrow().player_destroys, 1);
    });
}
#[test]
fn timeout_rejects_infinity_and_rounds_fractional_milliseconds_up() {
    assert_eq!(timeout_ms(Duration::from_nanos(1)).unwrap(), 1);
    assert_eq!(timeout_ms(Duration::ZERO).unwrap(), 0);
    assert!(timeout_ms(Duration::from_millis(u32::MAX as u64)).is_err());
}
