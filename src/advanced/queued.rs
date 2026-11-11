//! Native completion ownership for bounded asynchronous video queues.

use super::{ffi, AdvancedConfig, CompressedVideoFrame};
use crate::{ndi_lib::*, Error, Result, Sender, SenderOptions, VideoFrame, NDI};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::c_void,
    fmt,
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::{Duration, Instant},
};

type Callback = unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t);
type SetCallback = unsafe extern "C" fn(NDIlib_send_instance_t, *mut c_void, Option<Callback>);
type Submit = unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_video_frame_v2_t);

/// Limits include completed frames until the application collects them.
#[derive(Debug, Clone, Copy)]
pub struct QueueOptions {
    frames: usize,
    bytes: usize,
}
impl Default for QueueOptions {
    fn default() -> Self {
        Self {
            frames: 8,
            bytes: 512 * 1024 * 1024,
        }
    }
}
impl QueueOptions {
    /// Set the maximum number of retained submissions, including completed ones.
    pub fn max_frames(mut self, frames: usize) -> Self {
        self.frames = frames;
        self
    }
    /// Set the maximum retained pixel/packet and metadata allocation bytes.
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }
    fn validate(self) -> Result<Self> {
        if self.frames == 0
            || self.frames > 65_536
            || self.bytes == 0
            || self.bytes > isize::MAX as usize
        {
            return Err(Error::InvalidConfiguration(
                "queue requires 1..=65536 frames and a positive addressable byte limit".into(),
            ));
        }
        Ok(self)
    }
}
/// A unique submission identifier within a queue, never recycled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionId(u64);
impl SubmissionId {
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Owned video accepted by native completion sending.
#[derive(Debug)]
#[non_exhaustive]
pub enum QueuedVideoFrame {
    Pixels(VideoFrame),
    Compressed(CompressedVideoFrame),
}
impl From<VideoFrame> for QueuedVideoFrame {
    fn from(v: VideoFrame) -> Self {
        Self::Pixels(v)
    }
}
impl From<CompressedVideoFrame> for QueuedVideoFrame {
    fn from(v: CompressedVideoFrame) -> Self {
        Self::Compressed(v)
    }
}
impl QueuedVideoFrame {
    fn raw(&self) -> NDIlib_video_frame_v2_t {
        match self {
            Self::Pixels(v) => v.to_raw(),
            Self::Compressed(v) => v.raw(),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Pixels(v) => v.retained_bytes(),
            Self::Compressed(v) => {
                v.bytes.len()
                    + v.metadata
                        .as_ref()
                        .map_or(0, |m| m.as_bytes_with_nul().len())
            }
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Pixels(_) => Ok(()),
            Self::Compressed(v) => v.validate_for_send(),
        }
    }
}
/// Ownership returned when a submission cannot be accepted.
#[derive(Debug)]
pub struct QueueSubmitError {
    error: Error,
    frame: QueuedVideoFrame,
}
impl QueueSubmitError {
    pub fn error(&self) -> &Error {
        &self.error
    }
    pub fn into_parts(self) -> (Error, QueuedVideoFrame) {
        (self.error, self.frame)
    }
}
impl fmt::Display for QueueSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for QueueSubmitError {}
/// A native-completed submission whose storage can be reused.
#[derive(Debug)]
pub struct CompletedVideo {
    id: SubmissionId,
    frame: QueuedVideoFrame,
}
impl CompletedVideo {
    pub fn id(&self) -> SubmissionId {
        self.id
    }
    pub fn frame(&self) -> &QueuedVideoFrame {
        &self.frame
    }
    pub fn into_frame(self) -> QueuedVideoFrame {
        self.frame
    }
}

struct Entry {
    id: SubmissionId,
    header: Box<NDIlib_video_frame_v2_t>,
    frame: QueuedVideoFrame,
    bytes: usize,
    complete: bool,
}
// SAFETY: Header pointers refer only to this entry's immutable owned allocations.
// A callback never dereferences or drops those allocations; the locked registry
// keeps them alive until completion and the submission call has returned.
unsafe impl Send for Entry {}
struct State {
    entries: BTreeMap<usize, Entry>,
    ready: VecDeque<usize>,
    bytes: usize,
    next: u64,
    options: QueueOptions,
}
struct CallbackState {
    state: Mutex<State>,
    changed: Condvar,
    protocol_error: AtomicBool,
}
impl CallbackState {
    fn new(options: QueueOptions) -> Result<Box<Self>> {
        let options = options.validate()?;
        let mut ready = VecDeque::new();
        ready.try_reserve_exact(options.frames).map_err(|_| {
            Error::InvalidConfiguration("unable to allocate bounded completion storage".into())
        })?;
        Ok(Box::new(Self {
            state: Mutex::new(State {
                entries: BTreeMap::new(),
                ready,
                bytes: 0,
                next: 1,
                options,
            }),
            changed: Condvar::new(),
            protocol_error: AtomicBool::new(false),
        }))
    }
    fn complete(&self, address: usize) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match state.entries.get_mut(&address) {
            Some(entry) if !entry.complete => {
                entry.complete = true;
                // Capacity was reserved for every retained entry before native
                // registration. No allocator or user callback runs here.
                state.ready.push_back(address);
                self.changed.notify_all();
            }
            _ => {
                self.protocol_error.store(true, Ordering::Release);
                self.changed.notify_all();
            }
        }
    }
}
unsafe extern "C" fn completion(opaque: *mut c_void, frame: *const NDIlib_video_frame_v2_t) {
    if opaque.is_null() {
        return;
    }
    // SDK send_destroy is the documented callback drain barrier. The owning
    // QueuedSender retains this Box until that native destruction has returned.
    let state = unsafe { &*opaque.cast::<CallbackState>() };
    if frame.is_null() {
        state.protocol_error.store(true, Ordering::Release);
        state.changed.notify_all();
        return;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Header addresses may be reused by the SDK. Match the owned data
        // allocation, which remains unique throughout its retained submission.
        state.complete(unsafe { (*frame).p_data } as usize);
    }));
    if result.is_err() {
        state.protocol_error.store(true, Ordering::Release);
        state.changed.notify_all();
    }
}

/// A bounded, thread-affine sender using native completion callbacks.
///
/// Submission does not wait for the previous frame. No application callback runs
/// on an SDK thread: collect owned frames with try_take_completed/wait_completed.
/// Destruction blocks until the SDK drains callbacks; no deadline is promised.
pub struct QueuedSender {
    sender: Option<Sender>,
    callback: Box<CallbackState>,
    submit: Submit,
    _thread: PhantomData<Rc<()>>,
}
impl QueuedSender {
    /// Create a new sender and install completion ownership before any submission.
    pub fn new(ndi: &NDI, options: &SenderOptions, limits: QueueOptions) -> Result<Self> {
        let register: SetCallback =
            unsafe { ffi::required(ndi, b"NDIlib_send_set_video_async_completion\0")? };
        let submit: Submit = unsafe { ndi.resolve_optional(b"NDIlib_send_send_video_async_v2\0")? }
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "queued video sending".into(),
                reason: "native async submission unavailable".into(),
            })?;
        let callback = CallbackState::new(limits)?;
        let sender = Sender::new(ndi, options)?;
        Ok(Self::install(sender, callback, submit, register))
    }
    /// Create with retained Advanced JSON configuration.
    pub fn new_with_config(
        ndi: &NDI,
        options: &SenderOptions,
        config: &AdvancedConfig,
        limits: QueueOptions,
    ) -> Result<Self> {
        let register: SetCallback =
            unsafe { ffi::required(ndi, b"NDIlib_send_set_video_async_completion\0")? };
        let submit = unsafe { ffi::required(ndi, b"NDIlib_send_send_video_async_v2\0")? };
        let callback = CallbackState::new(limits)?;
        let sender = Sender::new_with_config(ndi, options, config)?;
        Ok(Self::install(sender, callback, submit, register))
    }
    /// Drain and consume an existing sender. Active advertiser registrations are
    /// rejected because they would defer the native destruction barrier.
    pub fn from_sender(mut sender: Sender, limits: QueueOptions) -> Result<Self> {
        let register = unsafe {
            ffi::required(
                sender.runtime(),
                b"NDIlib_send_set_video_async_completion\0",
            )?
        };
        let submit =
            unsafe { ffi::required(sender.runtime(), b"NDIlib_send_send_video_async_v2\0")? };
        let callback = CallbackState::new(limits)?;
        if !sender.has_exclusive_native_owner() {
            return Err(Error::InvalidConfiguration(
                "queued conversion requires a sender without advertiser registrations".into(),
            ));
        }
        drop(sender.finish_video_async());
        sender.clear_async_video_done();
        Ok(Self::install(sender, callback, submit, register))
    }
    fn install(
        sender: Sender,
        mut callback: Box<CallbackState>,
        submit: Submit,
        register: SetCallback,
    ) -> Self {
        unsafe {
            register(
                sender.native_instance(),
                (&mut *callback as *mut CallbackState).cast(),
                Some(completion),
            )
        };
        Self {
            sender: Some(sender),
            callback,
            submit,
            _thread: PhantomData,
        }
    }
    /// Submit without copying. On backpressure or validation failure, recover the
    /// original frame from QueueSubmitError. Completed frames count toward limits.
    // Backpressure returns the original owner without adding a fallible heap
    // allocation on an already capacity-constrained submission path.
    #[allow(clippy::result_large_err)]
    pub fn try_submit(
        &mut self,
        frame: impl Into<QueuedVideoFrame>,
    ) -> std::result::Result<SubmissionId, QueueSubmitError> {
        let frame = frame.into();
        let failure = |error, frame| QueueSubmitError { error, frame };
        if let Err(error) = frame.validate() {
            return Err(failure(error, frame));
        }
        if self.callback.protocol_error.load(Ordering::Acquire) {
            return Err(failure(
                Error::InvalidConfiguration(
                    "native completion protocol failed; drain and close this queue".into(),
                ),
                frame,
            ));
        }
        let bytes = frame.retained_bytes();
        let header = Box::new(frame.raw());
        let key = header.p_data as usize;
        let pointer = &*header as *const NDIlib_video_frame_v2_t;
        let id = {
            let mut state = self
                .callback
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let Some(total) = state
                .bytes
                .checked_add(bytes)
                .filter(|total| *total <= state.options.bytes)
            else {
                return Err(failure(
                    Error::InvalidConfiguration("queued video byte limit reached".into()),
                    frame,
                ));
            };
            if state.entries.len() >= state.options.frames {
                return Err(failure(
                    Error::InvalidConfiguration("queued video frame limit reached".into()),
                    frame,
                ));
            }
            if key == 0 || state.entries.contains_key(&key) {
                return Err(failure(
                    Error::InvalidFrame(
                        "queued frames require distinct owned data allocations".into(),
                    ),
                    frame,
                ));
            }
            let Some(next) = state.next.checked_add(1) else {
                return Err(failure(
                    Error::InvalidConfiguration("queue submission identifiers exhausted".into()),
                    frame,
                ));
            };
            let id = SubmissionId(state.next);
            state.next = next;
            state.bytes = total;
            state.entries.insert(
                key,
                Entry {
                    id,
                    header,
                    frame,
                    bytes,
                    complete: false,
                },
            );
            id
        };
        // Publish ownership before FFI, release the lock before the SDK can call
        // synchronously back, and retain the boxed header until collection.
        unsafe {
            (self.submit)(
                self.sender.as_ref().expect("live queue").native_instance(),
                pointer,
            )
        };
        Ok(id)
    }
    /// Retrieve one native-completed frame without waiting.
    pub fn try_take_completed(&mut self) -> Option<CompletedVideo> {
        let mut state = self
            .callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        take_completed(&mut state)
    }
    /// Wait for a completion within a finite caller budget. A timeout retains all
    /// frames and permits subsequent collection or continued use.
    pub fn wait_completed(&mut self, timeout: Duration) -> Result<Option<CompletedVideo>> {
        crate::to_ms_checked(timeout)?;
        let start = Instant::now();
        let mut state = self
            .callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(frame) = take_completed(&mut state) {
                return Ok(Some(frame));
            }
            if self.callback.protocol_error.load(Ordering::Acquire) {
                return Err(Error::InvalidConfiguration(
                    "native completion protocol failed".into(),
                ));
            }
            let Some(remaining) = timeout
                .checked_sub(start.elapsed())
                .filter(|v| !v.is_zero())
            else {
                return Ok(None);
            };
            let (next, _) = self
                .callback
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = next;
        }
    }
    /// Count native-pending plus completed-but-uncollected frames.
    pub fn retained_frames(&self) -> usize {
        self.callback
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .len()
    }
    /// Whether the native callback stream violated the expected one-completion contract.
    pub fn completion_protocol_failed(&self) -> bool {
        self.callback.protocol_error.load(Ordering::Acquire)
    }
}
fn take_completed(state: &mut State) -> Option<CompletedVideo> {
    let key = state.ready.pop_front()?;
    let entry = state
        .entries
        .remove(&key)
        .expect("completion names retained entry");
    state.bytes -= entry.bytes;
    // Drop the boxed header only after native completion and submission return.
    drop(entry.header);
    Some(CompletedVideo {
        id: entry.id,
        frame: entry.frame,
    })
}
impl Drop for QueuedSender {
    fn drop(&mut self) {
        // No registry lock held: send_destroy may run completion callbacks here.
        // Sender is exclusive and its native resource cannot outlive this call.
        drop(self.sender.take());
        // callback and all uncollected buffers drop only after the SDK barrier.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn insert(state: &CallbackState, id: u64) -> usize {
        let frame =
            QueuedVideoFrame::Pixels(VideoFrame::builder().resolution(2, 2).build().unwrap());
        let header = Box::new(frame.raw());
        let key = header.p_data as usize;
        let bytes = frame.retained_bytes();
        let mut s = state.state.lock().unwrap();
        s.bytes += bytes;
        s.entries.insert(
            key,
            Entry {
                id: SubmissionId(id),
                header,
                frame,
                bytes,
                complete: false,
            },
        );
        key
    }
    #[test]
    fn concurrent_and_out_of_order_callbacks_preserve_owned_frames() {
        let state = CallbackState::new(QueueOptions::default()).unwrap();
        let a = insert(&state, 1);
        let b = insert(&state, 2);
        state.complete(b);
        std::thread::scope(|scope| {
            scope.spawn(|| state.complete(a));
        });
        let mut s = state.state.lock().unwrap();
        assert_eq!(s.entries.len(), 2);
        assert_eq!(take_completed(&mut s).unwrap().id().get(), 2);
        assert_eq!(take_completed(&mut s).unwrap().id().get(), 1);
        assert_eq!(s.bytes, 0);
    }
    #[test]
    fn duplicate_and_unknown_completions_do_not_release_storage() {
        let state = CallbackState::new(QueueOptions::default()).unwrap();
        let a = insert(&state, 1);
        state.complete(a);
        state.complete(a);
        state.complete(0);
        let mut s = state.state.lock().unwrap();
        assert_eq!(s.ready.len(), 1);
        assert_eq!(s.entries.len(), 1);
        assert!(state.protocol_error.load(Ordering::Acquire));
        assert!(take_completed(&mut s).is_some());
        assert!(take_completed(&mut s).is_none());
    }
}

#[cfg(test)]
#[path = "queued_boundary_tests.rs"]
mod boundary_tests;
