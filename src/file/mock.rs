//! Inert File entry points for unit tests.
//!
//! A test replaces the few entries it cares about and keeps the rest:
//! `sys::FileRecording { NDIlib_file_recorder_stop: my_stop, ..mock::recording() }`.

use crate::sys::{self, bindings::*};

/// Define a constructor for a table whose every entry does nothing and returns
/// zero, false or null. The table's own type checks each signature.
macro_rules! inert_table {
    (
        $(#[$meta:meta])*
        fn $constructor:ident() -> $Table:ident {
            $(fn $name:ident($($type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    ) => {
        $(#[$meta])*
        pub(super) fn $constructor() -> sys::$Table {
            $(
                #[allow(non_snake_case)]
                unsafe extern "C" fn $name($(_: $type),*) $(-> $return)? {
                    // SAFETY: every return type is an integer, a boolean or a
                    // raw pointer, for which all-zero bytes are a valid value.
                    unsafe { std::mem::zeroed() }
                }
            )+
            sys::$Table { $($name,)+ }
        }
    };
}

inert_table! {
    /// Playback entry points that do nothing.
    fn playback() -> FilePlayback {
        fn NDIlib_file_player_probe_file(*const ::std::os::raw::c_char, *mut NDIlib_file_player_file_info_t) -> bool;
        fn NDIlib_file_player_create(*const NDIlib_file_player_create_t) -> NDIlib_file_player_instance_t;
        fn NDIlib_file_player_destroy(NDIlib_file_player_instance_t);
        fn NDIlib_file_player_start(NDIlib_file_player_instance_t, *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_player_stop(NDIlib_file_player_instance_t);
        fn NDIlib_file_player_is_running(NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_capture_event(NDIlib_file_player_instance_t, *mut NDIlib_file_player_event_t, u32) -> NDIlib_file_player_event_type_e;
        fn NDIlib_file_player_get_dropped_event_count(NDIlib_file_player_instance_t) -> u64;
        fn NDIlib_file_player_get_stats(NDIlib_file_player_instance_t, *mut NDIlib_file_player_stats_t) -> bool;
        fn NDIlib_file_player_seek_to_time(NDIlib_file_player_instance_t, u64) -> bool;
        fn NDIlib_file_player_seek_to_frame(NDIlib_file_player_instance_t, u64) -> bool;
        fn NDIlib_file_player_set_paused(NDIlib_file_player_instance_t, bool);
        fn NDIlib_file_player_get_paused(NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_set_loop(NDIlib_file_player_instance_t, bool);
        fn NDIlib_file_player_get_loop(NDIlib_file_player_instance_t) -> bool;
        fn NDIlib_file_player_get_current_duration(NDIlib_file_player_instance_t, *mut u64, *mut bool) -> bool;
        fn NDIlib_file_player_get_current_position(NDIlib_file_player_instance_t, *mut u64) -> bool;
        fn NDIlib_file_player_get_current_source_timecode(NDIlib_file_player_instance_t, *mut i64) -> bool;
        fn NDIlib_file_player_get_source_format(NDIlib_file_player_instance_t, *mut NDIlib_file_source_format_t) -> bool;
        fn NDIlib_file_player_get_advertised_source(NDIlib_file_player_instance_t) -> *const NDIlib_source_t;
    }
}

inert_table! {
    /// Recording entry points that do nothing.
    fn recording() -> FileRecording {
        fn NDIlib_file_recorder_create(*const NDIlib_file_recorder_create_t) -> NDIlib_file_recorder_instance_t;
        fn NDIlib_file_recorder_destroy(NDIlib_file_recorder_instance_t);
        fn NDIlib_file_recorder_start(NDIlib_file_recorder_instance_t, *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_recorder_start_with_timecode(NDIlib_file_recorder_instance_t, *const ::std::os::raw::c_char, i64) -> bool;
        fn NDIlib_file_recorder_capture_event(NDIlib_file_recorder_instance_t, *mut NDIlib_file_recorder_event_t, u32) -> NDIlib_file_recorder_event_type_e;
        fn NDIlib_file_recorder_get_dropped_event_count(NDIlib_file_recorder_instance_t) -> u64;
        fn NDIlib_file_recorder_stop(NDIlib_file_recorder_instance_t);
        fn NDIlib_file_recorder_rollover(NDIlib_file_recorder_instance_t, *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_file_recorder_inject_metadata(NDIlib_file_recorder_instance_t, *const NDIlib_metadata_frame_t) -> bool;
        fn NDIlib_file_recorder_set_source(NDIlib_file_recorder_instance_t, *const NDIlib_file_recorder_source_t) -> bool;
        fn NDIlib_file_recorder_is_recording(NDIlib_file_recorder_instance_t) -> bool;
        fn NDIlib_file_recorder_get_stats(NDIlib_file_recorder_instance_t, *mut NDIlib_file_recorder_stats_t) -> bool;
        fn NDIlib_file_recorder_get_source_format(NDIlib_file_recorder_instance_t, *mut NDIlib_file_source_format_t) -> bool;
    }
}

/// A recorder event of `kind` for the file at `path`.
pub(super) fn recorder_event(
    kind: NDIlib_file_recorder_event_type_e,
    path: &str,
) -> NDIlib_file_recorder_event_t {
    let mut event = NDIlib_file_recorder_event_t {
        type_: kind,
        ..Default::default()
    };
    for (slot, byte) in event.p_path.iter_mut().zip(path.bytes()) {
        *slot = byte as std::ffi::c_char;
    }
    event
}

pub(super) const STARTED: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_started;
pub(super) const ROLLOVER: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_rollover;
pub(super) const FILE_CLOSED: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed;
pub(super) const OPEN_FAILED: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_open_failed;
pub(super) const ERROR: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_error;
pub(super) const FIRST_FRAME: NDIlib_file_recorder_event_type_e =
    NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_first_frame;

/// When a [`Recorder`] reports its open file closed after a stop.
pub(super) enum Closing {
    /// Before the stop returns, as the 6.4 SDK does.
    AtStop,
    /// This long after the stop returns, as its documentation allows.
    After(std::time::Duration),
    /// After the stop returns, once the test calls [`Recorder::close`]:
    /// `After` with the delay in the test's hands, so that it can look at the
    /// stopping session first however slowly its threads run.
    WhenTold,
    /// Before the stop returns, which takes this long, as the 6.4 SDK's stop
    /// does while it finalizes a file.
    DuringSlowStop(std::time::Duration),
    /// The second duration after a stop that itself takes the first.
    AfterSlowStop(std::time::Duration, std::time::Duration),
    /// Never: the event is lost.
    Never,
}

/// A metadata frame as the wrapper handed it to the SDK.
pub(super) struct InjectedFrame {
    /// The address of its text: the wrapper's own storage, unless it copied.
    pub(super) address: usize,
    pub(super) length: i32,
    pub(super) timecode: i64,
    /// The `length` bytes at that address, read during the call.
    pub(super) bytes: Vec<u8>,
}

/// A stand-in for the SDK's recorder: an event queue the test can feed, and a
/// record of how it was stopped and destroyed.
pub(super) struct Recorder {
    closing: Closing,
    events: std::sync::Mutex<std::collections::VecDeque<NDIlib_file_recorder_event_t>>,
    changed: std::sync::Condvar,
    recording: std::sync::atomic::AtomicBool,
    /// A session was started and no stop call has followed it. As in the real
    /// library, this holds even after the session has ended by itself, and
    /// while it holds no further session can start.
    stop_owed: std::sync::atomic::AtomicBool,
    /// Refuse the next start with an open-failed event, as the real library
    /// does when it cannot create the first file.
    pub(super) fail_next_start: std::sync::atomic::AtomicBool,
    pub(super) refuse_creation: std::sync::atomic::AtomicBool,
    /// How many rollovers the wrapper passed on.
    pub(super) rollovers: std::sync::atomic::AtomicUsize,
    /// Every metadata frame the wrapper injected, in order.
    pub(super) injected: std::sync::Mutex<Vec<InjectedFrame>>,
    /// How many native recorders were created from this one, and how many
    /// starts and changes of source the wrapper passed on.
    pub(super) creates: std::sync::atomic::AtomicUsize,
    pub(super) starts: std::sync::atomic::AtomicUsize,
    pub(super) source_changes: std::sync::atomic::AtomicUsize,
    /// What creation was told about rolling over: whether a format change
    /// does not roll over, and the size that does.
    pub(super) created_without_format_rollover: std::sync::atomic::AtomicBool,
    pub(super) created_rollover_bytes: std::sync::atomic::AtomicU64,
    /// The filename pattern creation was given, if any, and whether it was
    /// told to leave out the proxy.
    pub(super) created_pattern: std::sync::Mutex<Option<String>>,
    pub(super) created_without_proxy: std::sync::atomic::AtomicBool,
    /// Make the next captured event larger than this crate's layout of it.
    pub(super) oversize_next_event: std::sync::atomic::AtomicBool,
    pub(super) dropped_events: std::sync::atomic::AtomicU64,
    pub(super) stops: std::sync::atomic::AtomicUsize,
    pub(super) destroys: std::sync::atomic::AtomicUsize,
    /// Whether the file-closed event had been captured when the recorder was destroyed.
    pub(super) closed_before_destroy: std::sync::atomic::AtomicBool,
    close_captured: std::sync::atomic::AtomicBool,
}

/// The path every [`Recorder`] session opens first.
pub(super) const TAKE: &str = "take.mkv";

thread_local! {
    /// The recorder the next creation on this thread hands out.
    static NEXT_RECORDER: std::cell::RefCell<Option<std::sync::Arc<Recorder>>> =
        const { std::cell::RefCell::new(None) };
}

impl Recorder {
    pub(super) fn new(closing: Closing) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            closing,
            events: Default::default(),
            changed: Default::default(),
            recording: Default::default(),
            stop_owed: Default::default(),
            fail_next_start: Default::default(),
            refuse_creation: Default::default(),
            rollovers: Default::default(),
            injected: Default::default(),
            creates: Default::default(),
            starts: Default::default(),
            source_changes: Default::default(),
            created_without_format_rollover: Default::default(),
            created_rollover_bytes: Default::default(),
            created_pattern: Default::default(),
            created_without_proxy: Default::default(),
            oversize_next_event: Default::default(),
            dropped_events: Default::default(),
            stops: Default::default(),
            destroys: Default::default(),
            closed_before_destroy: Default::default(),
            close_captured: Default::default(),
        })
    }

    /// Make this the recorder the next creation on the current thread returns.
    pub(super) fn install(self: &std::sync::Arc<Self>) {
        NEXT_RECORDER.with(|next| *next.borrow_mut() = Some(std::sync::Arc::clone(self)));
    }

    /// Queue an event of `kind` for `path`, as the SDK's own thread would.
    pub(super) fn push(&self, kind: NDIlib_file_recorder_event_type_e, path: &str) {
        self.push_event(recorder_event(kind, path));
    }

    /// Queue `event`, as the SDK's own thread would.
    pub(super) fn push_event(&self, event: NDIlib_file_recorder_event_t) {
        self.events.lock().unwrap().push_back(event);
        self.changed.notify_one();
    }

    /// End the session the way the library does on its own, for example after
    /// a format change it was told not to roll over for: the file is closed
    /// and an error event says why. No stop call is involved.
    pub(super) fn end_session_itself(&self) {
        self.recording
            .store(false, std::sync::atomic::Ordering::Release);
        self.push(FILE_CLOSED, TAKE);
        self.push(ERROR, TAKE);
    }

    /// Stop recording and report the first file closed: what a stop leads
    /// to, and what a test calls itself under [`Closing::WhenTold`].
    pub(super) fn close(&self) {
        self.recording
            .store(false, std::sync::atomic::Ordering::Release);
        self.push(FILE_CLOSED, TAKE);
    }

    /// [`close`](Self::close), `delay` from now, from a thread of its own.
    fn close_after(self: &std::sync::Arc<Self>, delay: std::time::Duration) {
        let recorder = std::sync::Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            recorder.close();
        });
    }

    /// The recorder behind a handle this module created.
    ///
    /// # Safety
    ///
    /// `handle` must come from [`recorder_create`] and not have been destroyed.
    unsafe fn from_handle<'a>(handle: NDIlib_file_recorder_instance_t) -> &'a std::sync::Arc<Self> {
        // SAFETY: the handle is the boxed `Arc` that creation leaked.
        unsafe { &*handle.cast::<std::sync::Arc<Self>>() }
    }
}

unsafe extern "C" fn recorder_create(
    settings: *const NDIlib_file_recorder_create_t,
) -> NDIlib_file_recorder_instance_t {
    use std::sync::atomic::Ordering;
    let recorder = NEXT_RECORDER
        .with(|next| next.borrow_mut().take())
        .expect("a mock recorder was installed on this thread");
    if recorder.refuse_creation.load(Ordering::Acquire) {
        return std::ptr::null_mut();
    }
    // SAFETY: the wrapper passes settings that are valid for the call.
    let settings = unsafe { &*settings };
    recorder.created_without_format_rollover.store(
        settings.disable_auto_rollover_on_format_change,
        Ordering::Release,
    );
    recorder
        .created_rollover_bytes
        .store(settings.rollover_max_file_size_bytes, Ordering::Release);
    *recorder.created_pattern.lock().unwrap() = (!settings.p_output_filename_pattern.is_null())
        .then(|| {
            // SAFETY: a pattern the wrapper passes is a NUL-terminated string
            // that is valid for the call.
            unsafe { std::ffi::CStr::from_ptr(settings.p_output_filename_pattern) }
                .to_str()
                .unwrap()
                .to_owned()
        });
    recorder
        .created_without_proxy
        .store(settings.disable_proxy_track, Ordering::Release);
    recorder.creates.fetch_add(1, Ordering::AcqRel);
    Box::into_raw(Box::new(recorder)).cast()
}

unsafe extern "C" fn recorder_destroy(handle: NDIlib_file_recorder_instance_t) {
    use std::sync::atomic::Ordering;
    // SAFETY: the wrapper destroys a handle it created exactly once.
    let recorder = unsafe { Box::from_raw(handle.cast::<std::sync::Arc<Recorder>>()) };
    recorder.closed_before_destroy.store(
        recorder.close_captured.load(Ordering::Acquire),
        Ordering::Release,
    );
    recorder.destroys.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "C" fn recorder_start(
    handle: NDIlib_file_recorder_instance_t,
    _: *const ::std::os::raw::c_char,
) -> bool {
    use std::sync::atomic::Ordering;
    // SAFETY: the wrapper passes a live handle.
    let recorder = unsafe { Recorder::from_handle(handle) };
    recorder.starts.fetch_add(1, Ordering::AcqRel);
    if recorder.fail_next_start.swap(false, Ordering::AcqRel) {
        recorder.push(OPEN_FAILED, "unopenable.mkv");
        return false;
    }
    if recorder.stop_owed.swap(true, Ordering::AcqRel) {
        return false;
    }
    recorder.recording.store(true, Ordering::Release);
    recorder.push(STARTED, TAKE);
    true
}

unsafe extern "C" fn recorder_capture(
    handle: NDIlib_file_recorder_instance_t,
    out: *mut NDIlib_file_recorder_event_t,
    wait_ms: u32,
) -> NDIlib_file_recorder_event_type_e {
    use std::sync::atomic::Ordering;
    // SAFETY: the wrapper passes a live handle.
    let recorder = unsafe { Recorder::from_handle(handle) };
    let mut events = recorder.events.lock().unwrap();
    if events.is_empty() && wait_ms != 0 {
        let wait = std::time::Duration::from_millis(wait_ms.into());
        (events, _) = recorder.changed.wait_timeout(events, wait).unwrap();
    }
    let Some(event) = events.pop_front() else {
        return NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_none;
    };
    if event.type_ == FILE_CLOSED {
        recorder.close_captured.store(true, Ordering::Release);
    }
    // SAFETY: the wrapper passes storage for one event, followed by its guard.
    unsafe {
        out.write(event);
        if recorder.oversize_next_event.swap(false, Ordering::AcqRel) {
            out.add(1).cast::<u8>().write_bytes(0, 16);
        }
    }
    event.type_
}

unsafe extern "C" fn recorder_stop(handle: NDIlib_file_recorder_instance_t) {
    use std::sync::atomic::Ordering;
    // SAFETY: the wrapper passes a live handle.
    let recorder = unsafe { Recorder::from_handle(handle) };
    recorder.stops.fetch_add(1, Ordering::AcqRel);
    recorder.stop_owed.store(false, Ordering::Release);
    // A session that has already ended by itself has nothing left to close.
    if !recorder.recording.load(Ordering::Acquire) {
        return;
    }
    match recorder.closing {
        Closing::AtStop => recorder.close(),
        Closing::After(delay) => recorder.close_after(delay),
        Closing::WhenTold => {}
        Closing::DuringSlowStop(delay) => {
            std::thread::sleep(delay);
            recorder.close();
        }
        Closing::AfterSlowStop(stop, delay) => {
            std::thread::sleep(stop);
            recorder.close_after(delay);
        }
        Closing::Never => recorder.recording.store(false, Ordering::Release),
    }
}

unsafe extern "C" fn recorder_is_recording(handle: NDIlib_file_recorder_instance_t) -> bool {
    // SAFETY: the wrapper passes a live handle.
    unsafe { Recorder::from_handle(handle) }
        .recording
        .load(std::sync::atomic::Ordering::Acquire)
}

unsafe extern "C" fn recorder_dropped(handle: NDIlib_file_recorder_instance_t) -> u64 {
    // SAFETY: the wrapper passes a live handle.
    unsafe { Recorder::from_handle(handle) }
        .dropped_events
        .load(std::sync::atomic::Ordering::Acquire)
}

unsafe extern "C" fn recorder_rollover(
    handle: NDIlib_file_recorder_instance_t,
    _: *const ::std::os::raw::c_char,
) -> bool {
    // SAFETY: the wrapper passes a live handle.
    unsafe { Recorder::from_handle(handle) }
        .rollovers
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    true
}

unsafe extern "C" fn recorder_inject_metadata(
    handle: NDIlib_file_recorder_instance_t,
    frame: *const NDIlib_metadata_frame_t,
) -> bool {
    // SAFETY: the wrapper passes a live handle, and a frame that is valid for
    // the call.
    let (recorder, frame) = unsafe { (Recorder::from_handle(handle), &*frame) };
    let length = usize::try_from(frame.length).expect("a length the SDK accepts");
    // SAFETY: the wrapper's frame points to `length` readable bytes, its
    // terminator included, for the call.
    let bytes = unsafe { std::slice::from_raw_parts(frame.p_data.cast::<u8>(), length) };
    recorder.injected.lock().unwrap().push(InjectedFrame {
        address: frame.p_data as usize,
        length: frame.length,
        timecode: frame.timecode,
        bytes: bytes.to_vec(),
    });
    true
}

unsafe extern "C" fn accept_source(
    handle: NDIlib_file_recorder_instance_t,
    _: *const NDIlib_file_recorder_source_t,
) -> bool {
    // SAFETY: the wrapper passes a live handle.
    unsafe { Recorder::from_handle(handle) }
        .source_changes
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    true
}

/// An API whose recorders are the [`Recorder`]s installed on the creating thread.
pub(super) fn recorder_api() -> super::FileApi {
    super::FileApi::with_tables(
        playback(),
        sys::FileRecording {
            NDIlib_file_recorder_create: recorder_create,
            NDIlib_file_recorder_destroy: recorder_destroy,
            NDIlib_file_recorder_start: recorder_start,
            NDIlib_file_recorder_capture_event: recorder_capture,
            NDIlib_file_recorder_get_dropped_event_count: recorder_dropped,
            NDIlib_file_recorder_stop: recorder_stop,
            NDIlib_file_recorder_rollover: recorder_rollover,
            NDIlib_file_recorder_inject_metadata: recorder_inject_metadata,
            NDIlib_file_recorder_set_source: accept_source,
            NDIlib_file_recorder_is_recording: recorder_is_recording,
            ..recording()
        },
    )
}
