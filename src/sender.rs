//! NDI sending functionality for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    finder::Source,
    frames::{
        AudioFrame, MetadataFrame, MetadataText, PixelFormat, ScanType, ValidatedVideoLayout,
        VideoFrame, VideoHeader,
    },
    receiver::Tally,
    sys::{self, bindings::*},
    timeout::Deadline,
    to_ms_checked, Error, FrameRate, Ndi, Rejected, Result, Timecode,
};

#[cfg(not(target_has_atomic = "ptr"))]
compile_error!(
    "This crate requires atomic pointer support. Please use a target with atomics enabled."
);

/// Flush the async video pipeline by passing a true NULL pointer.
///
/// The NDI SDK documentation specifies that calling
/// `NDIlib_send_send_video_async_v2(instance, NULL)` (where the *frame pointer*
/// is NULL, not merely a frame whose `p_data` is NULL) waits for any in-flight
/// async buffer to be released and then returns.
fn async_flush_frame_ptr() -> *const NDIlib_video_frame_v2_t {
    ptr::null()
}

/// The handle of a native sender: the one value through which this crate
/// reaches an SDK sender, and the one place where the sender types' thread
/// safety rests on the SDK's contract rather than on their Rust fields.
///
/// A copy is a borrowed handle. [`SenderResource`] owns the sender and
/// destroys it.
#[derive(Clone, Copy, Debug)]
struct SendInstance(NDIlib_send_instance_t);

// SAFETY: the handle is an opaque pointer with no thread affinity, and the SDK
// lets one sender be used from several threads at once. Its documentation of
// sending (the NDI-Send section) allows video, audio and metadata to be sent
// at any time, from any thread and in any order, and `Processing.NDI.Send.h`
// allows for audio and video submitted from separate threads when it
// describes their clocking. Nothing requires a sender to be destroyed on the
// thread that created it. A copy grants nothing by itself: every use is an
// `unsafe` SDK call made through an owner that keeps the sender alive, and
// `SenderResource` destroys it only when the last owner is released, so no
// call overlaps `NDIlib_send_destroy`. The operations this crate does not
// overlap are serialized by the Rust state around the handle, whose own types
// decide the auto traits of the types that hold it: the ownership and
// synchronization of asynchronous video (`Inner::video`), metadata capture
// with its matching free (`Inner::metadata_capture`), and the Advanced
// queue's completion state, which `QueuedSender` registers before it returns.
unsafe impl Send for SendInstance {}
// SAFETY: as for `Send`.
unsafe impl Sync for SendInstance {}

/// SDK instance and the owned storage retained while it sends video.
struct Inner {
    // A copy of the handle that `resource` owns.
    instance: SendInstance,
    // The entry points of the runtime that created `instance`.
    core: &'static sys::Core,
    // `None` only while `Inner::drop` runs.
    resource: Option<Arc<SenderResource>>,
    metadata_capture: parking_lot::Mutex<()>,
    video_callback: Option<AsyncCallback>,
    // Dropped after Inner::drop drains the SDK.
    video: Mutex<VideoState>,
}

/// A private ownership token; advertisements retain this without access to sends.
pub(crate) struct SenderResource {
    instance: SendInstance,
    _name: CString,
    _groups: Option<CString>,
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

impl SenderResource {
    /// The native sender this token keeps alive.
    pub(crate) fn instance(&self) -> NDIlib_send_instance_t {
        self.instance.0
    }
}

impl Drop for SenderResource {
    fn drop(&mut self) {
        // SAFETY: this token is the instance's final owner.
        unsafe { self._ndi.api().core.NDIlib_send_destroy(self.instance.0) };
    }
}

mod mode_sealed {
    pub trait Sealed {}
}

/// Sealed sender capability mode.
///
/// Every mode has the operations that do not depend on how video is sent:
/// the source, metadata in both directions, tally, connection count and
/// metadata, failover, and the Advanced encoder guidance, keyframe and tally
/// controls. [`RawSend`] alone sends uncompressed video; the modes that also
/// send uncompressed audio implement [`AudioSendMode`].
pub trait SenderMode: mode_sealed::Sealed + Send + Sync {}

/// Sealed sender modes that send uncompressed audio with
/// [`Sender::send_audio`]: [`RawSend`] and the queued mode.
pub trait AudioSendMode: SenderMode {}

/// Ordinary sender mode: uncompressed video, including the owned asynchronous
/// pipeline, audio and metadata. Only a sender in this mode can be registered
/// with a [`SendAdvertiser`](crate::discovery::SendAdvertiser).
#[derive(Debug)]
pub enum RawSend {}

/// Sender mode held exclusively by
/// [`CompressedSender`](crate::advanced::CompressedSender), which sends the
/// video and audio itself.
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub enum CompressedAttached {}

/// Sender mode held exclusively by
/// [`QueuedSender`](crate::advanced::QueuedSender), which sends the video
/// itself. It sends uncompressed audio.
#[cfg(feature = "advanced")]
#[derive(Debug)]
pub enum QueueAttached {}

impl mode_sealed::Sealed for RawSend {}
impl SenderMode for RawSend {}
impl AudioSendMode for RawSend {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for CompressedAttached {}
#[cfg(feature = "advanced")]
impl SenderMode for CompressedAttached {}
#[cfg(feature = "advanced")]
impl mode_sealed::Sealed for QueueAttached {}
#[cfg(feature = "advanced")]
impl SenderMode for QueueAttached {}
#[cfg(feature = "advanced")]
impl AudioSendMode for QueueAttached {}

/// An NDI source that sends video, audio and metadata.
///
/// Create one with [`Sender::new`]. Dropping the sender completes any
/// in-flight asynchronous video and removes the source from the network,
/// unless a [`SendAdvertiser`](crate::discovery::SendAdvertiser) registration
/// still holds it: a registered sender stays on the network until that
/// registration is removed or its advertiser is dropped.
///
/// The default mode, [`RawSend`], has every operation. The Advanced
/// `CompressedSender` and `QueuedSender` hold their sender in another
/// [`SenderMode`] and lend it through their `sender` method: it keeps the
/// operations every mode shares, but not the video sends its owner makes
/// itself, and it cannot be registered with an advertiser.
///
/// A sender is `Send` and `Sync`. Its operations take `&self` and may run on
/// several threads at once, except the asynchronous video submissions and the
/// completion handler setters, which take `&mut self`.
#[derive(Debug)]
pub struct Sender<M: SenderMode = RawSend> {
    inner: Inner,
    _ndi: Ndi,
    _mode: PhantomData<M>,
}

type AsyncCallback = Box<dyn Fn(usize) + Send + Sync>;

/// The sender's owned async video frame and whether the SDK may still read it.
///
/// A frame in flight always exists, so the state cannot say "in flight" without
/// the allocation the SDK is reading. A frame leaves `InFlight` only through
/// [`Self::completed`], which takes the proof that the SDK has synchronized,
/// or by being displaced by [`Self::submit`], which hands it back only with
/// that proof. Only a retained frame can be taken out.
#[derive(Default)]
enum VideoState {
    /// No frame is held.
    #[default]
    Idle,
    /// The SDK has released the frame; it awaits recovery.
    Retained(VideoFrame),
    /// The SDK may still be reading the frame.
    InFlight(VideoFrame),
}

impl VideoState {
    fn is_in_flight(&self) -> bool {
        matches!(self, Self::InFlight(_))
    }

    /// Mark the frame in flight as released and claim its notification,
    /// returning the pixel-buffer length to report.
    ///
    /// `_synchronized` proves that the SDK has returned from a synchronizing
    /// call made on this frame's sender after the frame was submitted, so no
    /// completion is reported before the call that ended the SDK's access.
    /// The notification is claimed before any user code runs, so a handler
    /// that panics or reenters cannot be notified twice.
    fn completed(&mut self, _synchronized: video_sync::Synchronized) -> Option<usize> {
        match std::mem::take(self) {
            Self::InFlight(frame) => {
                let len = frame.data().len();
                *self = Self::Retained(frame);
                Some(len)
            }
            other => {
                *self = other;
                None
            }
        }
    }

    /// Synchronize the SDK if it may still read the held frame, then complete
    /// it: the release a flush, a finish and sender destruction share.
    ///
    /// `instance` is the live sender, created through `core`, that this state
    /// belongs to.
    fn synchronize_and_complete(
        &mut self,
        core: &sys::Core,
        instance: SendInstance,
    ) -> Option<usize> {
        if !self.is_in_flight() {
            // A retained frame was completed when the SDK released it.
            return None;
        }
        let synchronized = self.flush(core, instance);
        self.completed(synchronized)
    }

    /// Remove the retained frame, leaving the state idle. A frame in flight
    /// stays held: it can be taken only once it has completed.
    fn take_retained(&mut self) -> Option<VideoFrame> {
        match std::mem::take(self) {
            Self::Retained(frame) => Some(frame),
            other => {
                *self = other;
                None
            }
        }
    }

    #[cfg(test)]
    fn frame(&self) -> Option<&VideoFrame> {
        match self {
            Self::Idle => None,
            Self::Retained(frame) | Self::InFlight(frame) => Some(frame),
        }
    }
}

/// The SDK calls that synchronize a sender's asynchronous video, and the proof
/// that one has returned.
///
/// The field of [`Synchronized`] is private to this module, so the rest of the
/// crate can obtain one only by making one of these calls, and each returns it
/// only after the SDK has returned. Each takes the sender's video state by
/// `&mut`, so its caller holds that state, by its lock or by `&mut`: one
/// sender's synchronizing calls never overlap, and different senders
/// synchronize independently.
///
/// Each takes `instance`, the live sender, created through `core`, that the
/// state belongs to. `Inner` holds all three, and its `SenderResource`
/// destroys the sender only after `Inner::drop` has drained it.
mod video_sync {
    use super::{async_flush_frame_ptr, BorrowedVideoFrame, SendInstance, VideoFrame, VideoState};
    use crate::sys;

    /// Proof that the SDK has returned from a call that synchronizes a
    /// sender's asynchronous video: an asynchronous or synchronous video send,
    /// or a null flush. `Processing.NDI.Send.h` and the [NDI-Send]
    /// documentation list these as synchronizing events: when one returns, the
    /// SDK no longer reads the frame previously submitted asynchronously.
    ///
    /// [`VideoState::completed`] consumes it, so a frame is reported complete
    /// only after the call that released it.
    ///
    /// [NDI-Send]: https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send
    #[must_use = "the proof completes the frame the SDK released"]
    pub(super) struct Synchronized(());

    impl VideoState {
        /// Wait for the SDK to release the frame in flight by passing a true
        /// NULL frame pointer, the SDK's documented request to drain the
        /// asynchronous buffer.
        pub(super) fn flush(&mut self, core: &sys::Core, instance: SendInstance) -> Synchronized {
            // SAFETY: `instance` is the live sender, created through `core`,
            // that this state belongs to, and a null frame pointer is the
            // SDK's documented request to drain the async buffer.
            unsafe { core.NDIlib_send_send_video_async_v2(instance.0, async_flush_frame_ptr()) };
            Synchronized(())
        }

        /// Send `frame` synchronously. When this returns, the SDK has
        /// finished reading `frame` and has released the frame in flight.
        pub(super) fn send(
            &mut self,
            core: &sys::Core,
            instance: SendInstance,
            frame: BorrowedVideoFrame<'_>,
        ) -> Synchronized {
            let raw = frame.to_raw();
            // SAFETY: `instance` is the live sender, created through `core`,
            // that this state belongs to. `raw` points into pixels and
            // metadata that `frame` borrows for longer than this call; the SDK
            // has finished reading them when the synchronous send returns and
            // keeps no reference afterwards.
            unsafe { core.NDIlib_send_send_video_v2(instance.0, &raw) };
            Synchronized(())
        }

        /// Make `frame` the frame in flight and hand it to the SDK. Returns
        /// the state it displaced, whose frame the SDK has released, with the
        /// proof of that release.
        pub(super) fn submit(
            &mut self,
            core: &sys::Core,
            instance: SendInstance,
            frame: VideoFrame,
        ) -> (VideoState, Synchronized) {
            let raw = frame.to_raw();
            // The sender owns the new frame before the SDK sees it. Moving a
            // frame does not move its pixel `Vec` or metadata `CString`
            // allocations, which `raw` points into.
            let displaced = std::mem::replace(self, Self::InFlight(frame));
            // SAFETY: `instance` is the live sender, created through `core`,
            // that this state belongs to. The SDK reads the new frame's
            // allocations after this call returns, until the next
            // synchronizing call: `self` owns them, and an in-flight frame
            // leaves it only with a later call's proof, while `Inner::drop`
            // drains the SDK before the state is dropped. A sender that is
            // never dropped leaks the frame rather than freeing it. Until
            // this call returns the SDK may still read the displaced frame:
            // `displaced` owns it across the call and is handed back only
            // with the proof the call produces, so it cannot be dropped
            // first.
            unsafe { core.NDIlib_send_send_video_async_v2(instance.0, &raw) };
            (displaced, Synchronized(()))
        }
    }
}

impl Inner {
    fn notify(&self, len: Option<usize>) {
        if let (Some(len), Some(callback)) = (len, self.video_callback.as_ref()) {
            callback(len);
        }
    }

    fn complete(&self) {
        let len = {
            let mut video = self.video.lock().unwrap_or_else(|e| e.into_inner());
            video.synchronize_and_complete(self.core, self.instance)
        };
        self.notify(len);
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance.0)
            .field("video_callback_set", &self.video_callback.is_some())
            .finish()
    }
}

/// A borrowed video frame that references external pixel data.
///
/// [`Sender::send_video`] sends it synchronously without copying, including
/// with padded rows; the buffer can be reused as soon as the call returns.
/// [`Sender::send_video_async_copy`] instead copies its pixels and metadata
/// into an owned frame for the asynchronous pipeline.
///
/// Create one with [`Self::builder`]. Construction borrows without
/// allocation and validates the entire supported uncompressed layout once.
/// Copying it copies only the borrows.
#[derive(Clone, Copy)]
pub struct BorrowedVideoFrame<'buf> {
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    /// Exactly the validated storage extent.
    data: &'buf [u8],
    /// The metadata text, validated with its terminator.
    metadata: Option<MetadataText<'buf>>,
}

impl crate::frames::VideoParts for BorrowedVideoFrame<'_> {
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.layout
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.header
    }
    /// `data` is exactly the storage the layout describes, as construction
    /// checked, initialized like any slice, and borrowed unchanged for
    /// `'buf`.
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.data.as_ptr()
    }
    #[inline]
    fn metadata_text(&self) -> Option<MetadataText<'_>> {
        self.metadata
    }
    /// A frame built for sending carries no timestamp; [`Self::to_raw`]
    /// sends none.
    #[inline]
    fn sdk_timestamp(&self) -> Option<crate::SdkTimestamp> {
        None
    }
}

crate::frames::video_accessors! {
    impl['buf] BorrowedVideoFrame<'buf>;
    debug("BorrowedVideoFrame", "data (bytes)" = data_len_bytes);
    timecode {}
    line_stride_bytes {
        /// It is the stride the frame was built with, which can include row
        /// padding.
    }
    plane {
        /// Its rows exclude the padding of the frame's line stride, which
        /// [`Self::data`] includes, so the active pixels of a padded buffer
        /// can be read in place.
    }
}

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Configure a checked borrowed frame over `data`.
    ///
    /// Its defaults are the packed line stride, 30/1 frames per second, a
    /// 16:9 picture aspect ratio, progressive scan, [`Timecode::SYNTHESIZE`]
    /// and no metadata. All validation occurs in
    /// [`BorrowedVideoFrameBuilder::build`].
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, FrameRate, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 4]; // BGRA buffer
    /// let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Bgra)
    ///     .frame_rate(FrameRate::new(30_000, 1_001)?)
    ///     .build()?;
    /// assert_eq!(frame.line_stride_bytes(), 1920 * 4);
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder(
        data: &'buf [u8],
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> BorrowedVideoFrameBuilder<'buf> {
        BorrowedVideoFrameBuilder {
            data,
            width,
            height,
            pixel_format,
            line_stride: None,
            frame_rate: const { FrameRate::per_second(30) },
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
        }
    }

    /// Copy this borrowed frame into a validated owned frame.
    ///
    /// Active pixels are copied into tightly packed planes; row padding is
    /// skipped and the owned stride can shrink. Metadata is copied separately;
    /// no reference to the source is retained. [`Self::copy_into`] makes the
    /// same copy into an existing frame, reusing its allocations.
    ///
    /// The copy cannot fail: building the frame validated everything it
    /// relies on, and the packed layout of a valid frame is valid.
    pub fn to_owned(&self) -> VideoFrame {
        crate::frames::VideoParts::owned_copy(self)
    }

    /// Copy this borrowed frame into `destination`, reusing its allocations.
    ///
    /// `destination` ends up exactly as [`Self::to_owned`] would have
    /// returned it, whatever it held before: tightly packed active rows,
    /// format, dimensions, frame rate, aspect ratio, scan type, timecode and
    /// metadata, and no timestamp. No reference to the source is retained.
    ///
    /// Its pixel buffer is reused when its capacity holds the packed frame,
    /// and otherwise replaced by one allocation of exactly that size. Its
    /// metadata string is reused when the new text has the same length, and
    /// otherwise the text takes one allocation. Copying frames of one shape
    /// into the same destination therefore allocates nothing after the first
    /// copy. The copy makes no SDK call and can run on any thread.
    ///
    /// # Example
    ///
    /// A copy-and-send loop that stops allocating once two frames exist.
    /// [`Sender::send_video_async`] keeps the frame it is given until the next
    /// send, and returns the previous one, which the SDK has finished with:
    /// that frame is the destination of the next copy.
    /// [`Sender::send_video_async_copy`] makes the same copy into a new frame
    /// on every call instead.
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, Ndi, PixelFormat, Sender, SenderOptions, VideoFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// let mut sender = Sender::new(&ndi, &SenderOptions::new("Copied"))?;
    /// // 64 pixels per row, padded to a 512-byte stride.
    /// let mut buffer = vec![0u8; 512 * 32];
    /// let mut spare: Option<VideoFrame> = None;
    /// for shade in 0..=255u8 {
    ///     buffer.fill(shade); // the application draws into its own buffer
    ///     let frame = BorrowedVideoFrame::builder(&buffer, 64, 32, PixelFormat::Bgra)
    ///         .line_stride(512)
    ///         .build()?;
    ///     let owned = match spare.take() {
    ///         Some(mut reused) => {
    ///             frame.copy_into(&mut reused); // no allocation
    ///             reused
    ///         }
    ///         None => frame.to_owned(), // the first two frames only
    ///     };
    ///     spare = sender.send_video_async(owned)?;
    /// }
    /// sender.finish_video_async();
    /// # Ok(())
    /// # }
    /// ```
    pub fn copy_into(&self, destination: &mut VideoFrame) {
        crate::frames::VideoParts::copy_into_owned(self, destination);
    }

    /// Get the pixel data: exactly the frame's validated storage, including
    /// any row padding. Bytes of the source buffer beyond the frame are not
    /// part of it. [`Self::plane`] and [`Self::planes`] borrow the active
    /// rows alone.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Return this frame with `timecode`.
    ///
    /// # Example
    ///
    /// ```
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Timecode};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 2];
    /// let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Uyvy)
    ///     .build()?
    ///     .with_timecode(Timecode::from_ticks(10_000_000));
    /// assert_eq!(frame.timecode().ticks(), 10_000_000);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_timecode(mut self, timecode: Timecode) -> Self {
        self.header.timecode = timecode;
        self
    }

    fn to_raw(self) -> NDIlib_video_frame_v2_t {
        crate::frames::sdk_video_frame(
            &self.layout,
            &self.header,
            self.data.as_ptr(),
            self.metadata.map_or(ptr::null(), MetadataText::as_ptr),
            None,
        )
    }
}

/// Builder for a [`BorrowedVideoFrame`], reached through
/// [`BorrowedVideoFrame::builder`]. No pixels or metadata are copied.
#[derive(Clone, Copy)]
pub struct BorrowedVideoFrameBuilder<'buf> {
    data: &'buf [u8],
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    line_stride: Option<usize>,
    frame_rate: FrameRate,
    picture_aspect_ratio: f32,
    scan_type: ScanType,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
}

impl<'buf> BorrowedVideoFrameBuilder<'buf> {
    /// Set the first-plane stride in bytes. Zero selects the packed stride.
    #[must_use]
    pub fn line_stride(mut self, stride: usize) -> Self {
        self.line_stride = Some(stride);
        self
    }
    /// Set the frame rate.
    #[must_use]
    pub fn frame_rate(mut self, frame_rate: FrameRate) -> Self {
        self.frame_rate = frame_rate;
        self
    }
    /// Set a finite nonnegative aspect ratio; zero selects square pixels.
    #[must_use]
    pub fn picture_aspect_ratio(mut self, ratio: f32) -> Self {
        self.picture_aspect_ratio = ratio;
        self
    }
    /// Select progressive, interlaced, or field scanning.
    #[must_use]
    pub fn scan_type(mut self, scan_type: ScanType) -> Self {
        self.scan_type = scan_type;
        self
    }
    /// Set the timecode.
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }
    /// Borrow NUL-terminated UTF-8 metadata, validated when built.
    #[must_use]
    pub fn metadata(mut self, metadata: &'buf CStr) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Validate dimensions, stride, initialized slice extent, headers, and metadata.
    ///
    /// Successful construction allocates nothing and never loads or queries
    /// an NDI runtime: whether the runtime can send the pixel format is
    /// checked when the frame is sent. The input slices remain borrowed.
    ///
    /// # Errors
    ///
    /// Invalid layout/header values, a buffer smaller than the layout, or
    /// metadata exceeding the 4 MiB cap return [`Error::InvalidFrame`];
    /// invalid metadata UTF-8 returns [`Error::InvalidUtf8`].
    pub fn build(self) -> Result<BorrowedVideoFrame<'buf>> {
        crate::frames::validate_picture_aspect_ratio(self.picture_aspect_ratio)?;
        let layout = ValidatedVideoLayout::new_uncompressed(
            self.pixel_format,
            self.width,
            self.height,
            self.line_stride,
        )?;
        let Some(data) = self.data.get(..layout.data_len_bytes) else {
            return Err(Error::InvalidFrame(format!(
                "Buffer too small for {:?}: got {} bytes, expected at least {}",
                self.pixel_format,
                self.data.len(),
                layout.data_len_bytes,
            )));
        };
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedVideoFrame {
            layout,
            header: VideoHeader {
                frame_rate: self.frame_rate,
                picture_aspect_ratio: self.picture_aspect_ratio,
                scan_type: self.scan_type,
                timecode: self.timecode,
            },
            data,
            metadata,
        })
    }
}

impl<'buf> From<&'buf VideoFrame> for BorrowedVideoFrame<'buf> {
    fn from(frame: &'buf VideoFrame) -> Self {
        BorrowedVideoFrame {
            layout: *crate::frames::VideoParts::layout(frame),
            header: *crate::frames::VideoParts::header(frame),
            data: frame.data(),
            metadata: crate::frames::VideoParts::metadata_text(frame),
        }
    }
}

impl<'a, 'buf> From<&'a BorrowedVideoFrame<'buf>> for BorrowedVideoFrame<'buf> {
    fn from(frame: &'a BorrowedVideoFrame<'buf>) -> Self {
        *frame
    }
}

impl Sender {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the name or groups contain a
    /// NUL byte, [`Error::UnsupportedCapability`] when the options carry an
    /// Advanced configuration that the runtime or this build cannot take, and
    /// [`Error::NativeOperationFailed`] when the SDK creates no sender.
    pub fn new(ndi: &Ndi, options: &SenderOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            return Self::new_configured(ndi, options, config);
        }
        // SAFETY: the SDK's sender creation returns null or a fresh sender
        // that nothing else owns, and the settings outlive the call.
        unsafe {
            Self::create(
                ndi,
                options,
                "NDIlib_send_create",
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.NDIlib_send_create(raw),
            )
        }
    }

    /// The one creation path: create a sender through `create`, the entry
    /// point `operation`, which is handed the core table and the native
    /// settings for `options`, and keep `config` for as long as the native
    /// sender lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live sender of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    pub(crate) unsafe fn create(
        ndi: &Ndi,
        options: &SenderOptions,
        operation: &'static str,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static sys::Core, &NDIlib_send_create_t) -> NDIlib_send_instance_t,
    ) -> Result<Self> {
        let name = CString::new(options.name.as_str())?;
        let groups = options.groups.as_deref().map(CString::new).transpose()?;
        let core = &ndi.api().core;
        let instance = create(
            core,
            &NDIlib_send_create_t {
                p_ndi_name: name.as_ptr(),
                p_groups: groups.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                clock_video: options.clock_video,
                clock_audio: options.clock_audio,
            },
        );
        if instance.is_null() {
            return Err(Error::NativeOperationFailed { operation });
        }
        let instance = SendInstance(instance);
        Ok(Self {
            inner: Inner {
                instance,
                core,
                resource: Some(Arc::new(SenderResource {
                    instance,
                    _name: name,
                    _groups: groups,
                    #[cfg(feature = "advanced")]
                    _config: config,
                    _ndi: ndi.clone(),
                })),
                metadata_capture: parking_lot::Mutex::new(()),
                video_callback: None,
                video: Mutex::new(VideoState::default()),
            },
            _ndi: ndi.clone(),
            _mode: PhantomData,
        })
    }

    /// A new owner of the native sender, which keeps it alive after this
    /// value is dropped. Only an ordinary sender hands one out: the wrappers
    /// that hold a sender in another mode free what the SDK reads once their
    /// native sender is gone, so it must not outlive them.
    pub(crate) fn native_owner(&self) -> Arc<SenderResource> {
        Arc::clone(
            self.inner
                .resource
                .as_ref()
                .expect("live sender owns its resource"),
        )
    }

    #[cfg(feature = "advanced")]
    pub(crate) fn has_exclusive_native_owner(&self) -> bool {
        Arc::strong_count(
            self.inner
                .resource
                .as_ref()
                .expect("live sender owns its resource"),
        ) == 1
    }

    /// Send a video frame synchronously, without copying it.
    ///
    /// Accepts `&VideoFrame`, a [`BorrowedVideoFrame`], or `&BorrowedVideoFrame`.
    /// The SDK reads the pixels and metadata in place: nothing is allocated
    /// or copied, including for a borrowed frame with padded rows. This is
    /// the call to use for borrowed or padded buffers; unlike
    /// [`Self::send_video_async_copy`] it needs no owned copy.
    ///
    /// The SDK has released the input buffer when this returns; the caller may
    /// then reuse or release it. This also completes a previous async video send.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] if this sender's runtime
    /// cannot send the frame's pixel format (the planar 4:4:4 formats need
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video)).
    /// Nothing is sent and no pending send is changed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, Ndi, PixelFormat, Sender, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let sender = Sender::new(&ndi, &SenderOptions::new("Padded"))?;
    /// // 64 pixels per row, padded to a 512-byte stride.
    /// let mut buffer = vec![0u8; 512 * 32];
    /// let frame = BorrowedVideoFrame::builder(&buffer, 64, 32, PixelFormat::Bgra)
    ///     .line_stride(512)
    ///     .build()?;
    /// sender.send_video(frame)?;
    /// buffer.fill(0xff); // the SDK is done with the buffer
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video<'a>(&self, frame: impl Into<BorrowedVideoFrame<'a>>) -> Result<()> {
        let frame: BorrowedVideoFrame<'a> = frame.into();
        self.require_video_format(frame.pixel_format())?;
        let len = {
            let mut video = self.inner.video.lock().unwrap_or_else(|e| e.into_inner());
            // A synchronous video send also releases the previous async frame,
            // which completes once the send has returned.
            let synchronized = video.send(self.inner.core, self.inner.instance, frame);
            video.completed(synchronized)
        };
        self.inner.notify(len);
        Ok(())
    }

    /// Submit an owned frame without copying its pixel or metadata allocations.
    ///
    /// The sender takes ownership before calling the SDK. The next async send
    /// synchronizes the previous send and returns its owned frame for reuse.
    /// The first send returns `None`. Owned and copied submissions share this
    /// recovery behavior. Call [`Self::finish_video_async`] to recover the final frame.
    ///
    /// Synchronous sends and explicit flushes complete the operation but retain
    /// the owned frame for recovery by the next async send or finish call.
    /// No SDK completion callback is registered, including with Advanced SDK.
    /// If a Rust completion handler panics, the new frame remains retained and
    /// the synchronized previous frame is dropped rather than returned.
    ///
    /// # Errors
    ///
    /// Returns the frame, unsent, with [`Error::UnsupportedCapability`] if
    /// this sender's runtime cannot send its pixel format (the planar 4:4:4
    /// formats need
    /// [`RuntimeFeature::Planar444Video`](crate::RuntimeFeature::Planar444Video)).
    /// No pending send is changed.
    pub fn send_video_async(
        &mut self,
        frame: VideoFrame,
    ) -> std::result::Result<Option<VideoFrame>, Rejected<VideoFrame>> {
        if let Err(error) = self.require_video_format(frame.pixel_format()) {
            return Err(Rejected::new(error, frame));
        }
        Ok(self.submit_video_async(frame))
    }

    /// Hand a checked frame to the SDK and return the previous one, if any.
    fn submit_video_async(&mut self, frame: VideoFrame) -> Option<VideoFrame> {
        let (previous, len) = {
            let mut video = self.inner.video.lock().unwrap_or_else(|e| e.into_inner());
            // The sender owns the new frame before the SDK sees it. The
            // previous frame stays owned by `displaced` until the submission,
            // which synchronizes the SDK's access to it, has returned; only
            // the proof of that return completes it.
            let (mut displaced, synchronized) =
                video.submit(self.inner.core, self.inner.instance, frame);
            let len = displaced.completed(synchronized);
            (displaced.take_retained(), len)
        };
        // State is consistent and the mutex unlocked before user code. If it
        // panics, the synchronized previous frame is safe to drop and the
        // sender retains the new frame.
        self.inner.notify(len);
        previous
    }

    /// Drain the SDK and return the final owned frame, if any.
    ///
    /// Frames submitted through either owned or copied sending are returned.
    /// Repeated calls return `None` and do not repeat completion notifications.
    /// If the completion handler panics, the completed frame is dropped instead
    /// of returned.
    pub fn finish_video_async(&mut self) -> Option<VideoFrame> {
        let (previous, len) = {
            let mut video = self.inner.video.lock().unwrap_or_else(|e| e.into_inner());
            let len = video.synchronize_and_complete(self.inner.core, self.inner.instance);
            (video.take_retained(), len)
        };
        self.inner.notify(len);
        previous
    }

    /// Copy borrowed pixels and metadata, then submit the owned copy asynchronously.
    ///
    /// This uses the same pipeline as [`Self::send_video_async`], returning the
    /// previous completed frame whether it was submitted by ownership or copying.
    /// The caller may immediately reuse or release the source buffer and metadata.
    /// Recover the final copy with [`Self::finish_video_async`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] if this sender's runtime
    /// cannot send the frame's pixel format, checked before anything is
    /// copied. No pending send is changed on failure.
    pub fn send_video_async_copy(
        &mut self,
        frame: &BorrowedVideoFrame<'_>,
    ) -> Result<Option<VideoFrame>> {
        self.require_video_format(frame.pixel_format())?;
        let owned = frame.to_owned();
        Ok(self.submit_video_async(owned))
    }

    /// Replace the completion handler receiving the submitted pixel-buffer length.
    ///
    /// It runs on the calling thread after SDK synchronization, outside internal
    /// locks, at most once per submitted frame. Completion is observed by the
    /// next video send, flush, finish, or sender destruction. Owned and copied sends share
    /// this handler. State is updated before invocation, so panic or reentry cannot
    /// repeat a notification. If sender destruction runs during unwinding, a
    /// handler panic is suppressed to avoid a second panic.
    ///
    /// Replaces the previous handler. The current handler observes completion of
    /// a frame still in flight, including one submitted before registration.
    /// Frames completed before registration are not notified retroactively.
    pub fn on_async_video_done<F>(&mut self, handler: F)
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.inner.video_callback = Some(Box::new(handler));
    }

    /// Remove the completion handler. Pending frame ownership is unchanged.
    pub fn clear_async_video_done(&mut self) {
        self.inner.video_callback = None;
    }

    /// Flush pending async video operations synchronously.
    ///
    /// Sends a true NULL video frame pointer to the SDK, blocking until all
    /// pending async video operations are complete.
    ///
    /// After this returns, the SDK has released the pending buffer. The sender
    /// retains the frame; use [`Self::finish_video_async`] or the next async
    /// submission to recover it without copying.
    pub fn flush_async_blocking(&self) {
        self.inner.complete();
    }
}

impl<M: AudioSendMode> Sender<M> {
    /// Sends an audio frame synchronously.
    ///
    /// The SDK has released the input buffer when this returns, so the caller
    /// may then reuse or release it. This calls `NDIlib_send_send_audio_v3`.
    /// It is available in the modes that implement [`AudioSendMode`]: on an
    /// ordinary sender and on the one a `QueuedSender` lends.
    ///
    /// See the NDI SDK documentation section on `NDIlib_send_send_audio_v3` for more details.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions, AudioFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let options = SenderOptions::new("Test");
    /// # let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let mut audio_buffer = vec![0.0f32; 48000 * 2]; // 1 second of stereo audio
    ///
    /// // Fill buffer with audio data...
    /// let frame = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .num_channels(2)
    ///     .num_samples(48000)
    ///     .data(audio_buffer.clone())
    ///     .build()?;
    /// sender.send_audio(&frame);
    ///
    /// // Buffer can be reused immediately
    /// audio_buffer.fill(0.5);
    /// let frame2 = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .num_channels(2)
    ///     .num_samples(48000)
    ///     .data(audio_buffer)
    ///     .build()?;
    /// sender.send_audio(&frame2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_audio(&self, audio_frame: &AudioFrame) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_send_audio_v3(self.inner.instance.0, &audio_frame.to_raw());
        }
    }
}

impl<M: SenderMode> Sender<M> {
    /// The same sender in mode `T`. The native sender, its options and its
    /// asynchronous video state are unchanged.
    #[cfg(feature = "advanced")]
    pub(crate) fn into_mode<T: SenderMode>(self) -> Sender<T> {
        Sender {
            inner: self.inner,
            _ndi: self._ndi,
            _mode: PhantomData,
        }
    }

    pub(crate) fn native_instance(&self) -> NDIlib_send_instance_t {
        self.inner.instance.0
    }

    /// The core entry points of the runtime that created this sender.
    #[cfg(feature = "advanced")]
    pub(crate) fn core(&self) -> &'static sys::Core {
        self.inner.core
    }

    pub(crate) fn runtime(&self) -> &Ndi {
        &self._ndi
    }

    /// Check that this sender's runtime can send `pixel_format`: the one
    /// check every video send makes before anything reaches the SDK.
    pub(crate) fn require_video_format(&self, pixel_format: PixelFormat) -> Result<()> {
        pixel_format.require_sendable(&self._ndi)
    }

    /// Capture metadata sent back by connected receivers.
    ///
    /// The result owns its data. The wait budget includes contention with other
    /// metadata captures and does not block the video-send synchronization lock.
    pub fn capture_metadata(&self, timeout: Duration) -> Result<Option<MetadataFrame>> {
        let metadata = self._ndi.table::<sys::SenderMetadata>()?;
        let deadline = Deadline::after(timeout)?;
        let Some(_lock) = self.inner.metadata_capture.try_lock_until(deadline.end()) else {
            return Ok(None);
        };
        let mut raw = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 0,
            p_data: ptr::null_mut(),
        };
        let remaining = deadline.remaining_ms();
        // SAFETY: the sender instance is live for this borrow and `raw` is
        // writable storage for one metadata descriptor.
        let kind =
            unsafe { metadata.NDIlib_send_capture(self.inner.instance.0, &mut raw, remaining) };
        if kind == NDIlib_frame_type_e_NDIlib_frame_type_metadata {
            /// Frees one frame that `NDIlib_send_capture` filled, through the
            /// sender and the runtime table that filled it, when dropped.
            struct Release {
                instance: NDIlib_send_instance_t,
                raw: NDIlib_metadata_frame_t,
                metadata: &'static sys::SenderMetadata,
            }
            impl Drop for Release {
                fn drop(&mut self) {
                    // SAFETY: a `Release` is built only below, right after
                    // `NDIlib_send_capture` reported that it filled `raw` on
                    // `instance` through `metadata`, the table of the runtime
                    // that created the sender. The guard never leaves this
                    // call, whose `&Sender` keeps the sender alive and whose
                    // capture lock is released only after the guard drops.
                    // This drop is the frame's only release: it is freed
                    // exactly once, through the same instance, after its last
                    // read.
                    unsafe {
                        self.metadata
                            .NDIlib_send_free_metadata(self.instance, &self.raw)
                    };
                }
            }
            // From here the guard owns the captured frame, so every exit frees
            // it: a validation error, a panic, or the owned copy's return.
            let guard = Release {
                instance: self.inner.instance.0,
                raw,
                metadata,
            };
            // SAFETY: `NDIlib_send_capture` filled `guard.raw` on this sender
            // and reported a metadata frame, which stays allocated until the
            // guard frees it. Its `p_data` is null or SDK storage of `length`
            // bytes ending in the NUL terminator, or a NUL-terminated string
            // when `length` is zero, as `Processing.NDI.structs.h` describes a
            // metadata frame; validation reads no further than that. The
            // text borrows `guard.raw`, so it is copied before the guard frees
            // it.
            let text = unsafe { crate::frames::validate_metadata_layout(&guard.raw)? };
            let timecode = Timecode::from_ticks(guard.raw.timecode);
            Ok(Some(MetadataFrame::copied(text, timecode)))
        } else if kind == NDIlib_frame_type_e_NDIlib_frame_type_none {
            Ok(None)
        } else {
            Err(Error::CaptureFailed(format!(
                "sender metadata capture returned {kind}"
            )))
        }
    }

    /// Sends a metadata frame.
    ///
    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) {
        let raw = metadata_frame.to_raw();
        // The borrowed C string remains live through this synchronous SDK call.
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_send_metadata(self.inner.instance.0, &raw);
        }
    }

    /// The tally this sender currently reports.
    ///
    /// Polls without waiting and returns the current state every time, whether
    /// or not it has changed since the last query. Call
    /// [`Self::wait_for_tally_change`] to block until it changes. Never fails
    /// and may be called from any thread.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// let tally = sender.tally();
    /// println!("On program: {}, On preview: {}", tally.on_program(), tally.on_preview());
    /// # Ok(())
    /// # }
    /// ```
    pub fn tally(&self) -> Tally {
        self.get_tally(0).1
    }

    /// Wait up to `timeout` for the tally to change.
    ///
    /// Returns `Ok(Some(tally))` with the new state when the SDK reports a
    /// change, and `Ok(None)` when `timeout` elapses first. Use
    /// [`Self::tally`] to read the current state without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// (about 49.7 days).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// if let Some(tally) = sender.wait_for_tally_change(Duration::from_secs(1))? {
    ///     println!("On program: {}, On preview: {}", tally.on_program(), tally.on_preview());
    /// } else {
    ///     println!("Tally did not change");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_tally_change(&self, timeout: Duration) -> Result<Option<Tally>> {
        let timeout_ms = to_ms_checked(timeout)?;
        let (changed, tally) = self.get_tally(timeout_ms);
        Ok(changed.then_some(tally))
    }

    /// Query the SDK's tally, waiting up to `timeout_ms`; returns whether it
    /// changed, and the tally.
    fn get_tally(&self, timeout_ms: u32) -> (bool, Tally) {
        let mut raw_tally = Tally::default().to_sdk();
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let changed = unsafe {
            self.inner
                .core
                .NDIlib_send_get_tally(self.inner.instance.0, &mut raw_tally, timeout_ms)
        };
        (
            changed,
            Tally::new(raw_tally.on_program, raw_tally.on_preview),
        )
    }

    /// Get the number of active connections to this sender.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for connection count.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// Number of active connections as a `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedSdkResult`] if the SDK returns a negative connection count.
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// // Get connection count with 1 second timeout
    /// let count = sender.connection_count(Duration::from_secs(1))?;
    /// println!("Active connections: {}", count);
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_count(&self, timeout: Duration) -> Result<u32> {
        let timeout_ms = to_ms_checked(timeout)?;
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let count = unsafe {
            self.inner
                .core
                .NDIlib_send_get_no_connections(self.inner.instance.0, timeout_ms)
        };

        u32::try_from(count).map_err(|_| {
            Error::UnexpectedSdkResult(format!(
                "NDIlib_send_get_no_connections returned a negative count: {count}"
            ))
        })
    }

    /// Remove all metadata registered for delivery to each new connection.
    pub fn clear_connection_metadata(&self) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_clear_connection_metadata(self.inner.instance.0)
        }
    }

    /// Adds connection metadata.
    ///
    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) {
        let raw = metadata_frame.to_raw();
        // The borrowed C string remains live through this synchronous SDK call.
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_add_connection_metadata(self.inner.instance.0, &raw)
        }
    }

    /// Sets failover source.
    ///
    /// # Errors
    ///
    /// Returns an error if source conversion fails.
    pub fn set_failover(&self, source: &Source) -> Result<()> {
        let raw_source = source.to_raw()?;
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_set_failover(self.inner.instance.0, &raw_source.raw)
        }
        Ok(())
    }

    /// Clear the failover source previously configured with [`Self::set_failover`].
    pub fn clear_failover(&self) {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        unsafe {
            self.inner
                .core
                .NDIlib_send_set_failover(self.inner.instance.0, ptr::null())
        }
    }

    /// Get the source information for this sender.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] when the SDK supplies no
    /// descriptor, or one that names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let options = SenderOptions::new("Test Sender");
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let source = sender.source()?;
    /// println!("Sender source: {source}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self) -> Result<Source> {
        // SAFETY: the sender instance is live for this borrow, and every pointer
        // passed outlives the synchronous call.
        let source_ptr = unsafe {
            self.inner
                .core
                .NDIlib_send_get_source_name(self.inner.instance.0)
        };
        // SAFETY: the SDK sender owns this live descriptor and its strings
        // through the synchronous copy performed here.
        unsafe { Source::try_from_raw(source_ptr, "NDIlib_send_get_source_name") }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let len = self
            .video
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .synchronize_and_complete(self.core, self.instance);
        // Destroy before running user code, so a callback panic cannot leak the
        // SDK instance. Pending allocations remain alive through destruction.
        // The native instance may intentionally outlive this facade when an
        // advertiser owns a registration. Pixels have already been drained.
        drop(self.resource.take());
        if std::thread::panicking() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.notify(len)));
        } else {
            self.notify(len);
        }
    }
}

/// Configuration for creating a [`Sender`].
///
/// The defaults pace video and audio submissions to their frame and sample
/// rates, in the groups of the NDI configuration.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{Ndi, SenderOptions, Sender};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// # let ndi = Ndi::new()?;
/// let options = SenderOptions::new("My Sender").groups("Studio");
/// let sender = Sender::new(&ndi, &options)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SenderOptions {
    name: String,
    groups: Option<String>,
    clock_video: bool,
    clock_audio: bool,
    #[cfg(feature = "advanced")]
    pub(crate) config: Option<crate::advanced::AdvancedConfig>,
}

impl SenderOptions {
    /// A sender advertised as `name`, which the SDK chooses when it is empty,
    /// with the default settings.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: None,
            clock_video: true,
            clock_audio: true,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }

    /// Set the comma-separated discovery groups the sender is advertised
    /// in, instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Set whether video submissions are paced to the frame rate, rather
    /// than returning at once.
    #[must_use]
    pub fn clock_video(mut self, clock: bool) -> Self {
        self.clock_video = clock;
        self
    }

    /// Set whether audio submissions are paced to the sample rate, rather
    /// than returning at once.
    #[must_use]
    pub fn clock_audio(mut self, clock: bool) -> Self {
        self.clock_audio = clock;
        self
    }

    /// Create the sender with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`Sender::new`] then needs an Advanced runtime, and a build with the
    /// Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeFeature;
    use std::sync::Arc;

    fn test_owned_frame(value: u8) -> VideoFrame {
        let mut frame = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::Bgra)
            .frame_rate(FrameRate::new(30, 1).unwrap())
            .metadata(format!("<frame value='{value}'/>"))
            .build()
            .unwrap();
        frame.data_mut().fill(value);
        frame
    }

    /// A sender on the real runtime, or `None` when the test must be skipped.
    fn test_sender() -> Option<Sender> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let name = crate::test_support::unique_name(&format!(
            "Owned async regression {}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let ndi = crate::test_support::ndi()?;
        Some(
            Sender::new(
                &ndi,
                &SenderOptions::new(name)
                    .clock_video(false)
                    .clock_audio(false),
            )
            .unwrap(),
        )
    }

    #[test]
    fn metadata_send_methods_reuse_validated_text_with_sdk() {
        let Some(sender) = test_sender() else {
            return;
        };
        let frame = MetadataFrame::new("<ndi_text>é</ndi_text>")
            .unwrap()
            .with_timecode(Timecode::from_ticks(42));
        let expected_ptr = frame.to_raw().p_data;
        for _ in 0..3 {
            sender.send_metadata(&frame);
            assert_eq!(frame.to_raw().p_data, expected_ptr);
        }
        sender.add_connection_metadata(&frame);
        sender.clear_connection_metadata();
    }

    #[test]
    fn owned_async_recycles_original_pixel_and_metadata_allocations() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let first = test_owned_frame(11);
        let pixels = first.data().as_ptr();
        let metadata = first.metadata_cstr().unwrap().as_ptr();
        assert!(sender.send_video_async(first).unwrap().is_none());
        {
            let video = sender.inner.video.lock().unwrap();
            let Some(frame) = video.frame() else {
                panic!("missing owned frame")
            };
            assert_eq!(frame.to_raw().p_data, pixels as *mut u8);
            assert_eq!(frame.to_raw().p_metadata, metadata);
        }
        let second = test_owned_frame(22);
        let second_pixels = second.data().as_ptr();
        let recycled = sender.send_video_async(second).unwrap().unwrap();
        assert_eq!(recycled.data().as_ptr(), pixels);
        assert_eq!(recycled.metadata_cstr().unwrap().as_ptr(), metadata);
        assert_eq!(recycled.metadata(), Some("<frame value='11'/>"));
        let final_frame = sender.finish_video_async().unwrap();
        assert_eq!(final_frame.data().as_ptr(), second_pixels);
        assert!(sender.finish_video_async().is_none());
    }

    #[test]
    fn synchronous_send_of_a_padded_borrowed_frame_allocates_nothing() {
        let Some(sender) = test_sender() else {
            return;
        };
        // 16 pixels per row, padded to a 96-byte stride.
        let data = vec![5u8; 96 * 16];
        let metadata = c"<padded/>";
        let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
            .line_stride(96)
            .metadata(metadata)
            .build()
            .unwrap();
        // Warm any lazily created SDK or runtime state before counting.
        sender.send_video(borrowed).unwrap();
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(borrowed).unwrap();
            // Lint allowed: the by-reference form is what is under test.
            #[allow(clippy::needless_borrows_for_generic_args)]
            sender.send_video(&borrowed).unwrap();
        });
        assert_eq!(allocations, 0);
        // An owned frame is lent the same way.
        let owned = test_owned_frame(3);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            sender.send_video(&owned).unwrap();
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn synchronous_send_describes_an_owned_frame_as_the_frame_itself() {
        let owned = test_owned_frame(8);
        let borrowed = BorrowedVideoFrame::from(&owned);
        let (expected, lent) = (owned.to_raw(), borrowed.to_raw());
        assert_eq!(lent.p_data, expected.p_data);
        assert_eq!(lent.p_metadata, expected.p_metadata);
        // SAFETY: both descriptors are uncompressed, so the union holds the stride.
        let strides = unsafe {
            (
                lent.__bindgen_anon_1.line_stride_in_bytes,
                expected.__bindgen_anon_1.line_stride_in_bytes,
            )
        };
        assert_eq!(strides.0, strides.1);
        assert_eq!(
            (lent.xres, lent.yres, lent.FourCC, lent.timecode),
            (
                expected.xres,
                expected.yres,
                expected.FourCC,
                expected.timecode
            )
        );
        assert_eq!(
            (lent.frame_rate_N, lent.frame_rate_D, lent.frame_format_type),
            (
                expected.frame_rate_N,
                expected.frame_rate_D,
                expected.frame_format_type
            )
        );
        assert_eq!(lent.picture_aspect_ratio, expected.picture_aspect_ratio);
    }

    #[test]
    fn mixed_sync_flush_and_copy_sends_notify_once_and_preserve_recovery() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(move |len| {
            assert_eq!(len, 1024);
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let frame = test_owned_frame(1);
        let pixels = frame.data().as_ptr();
        sender.send_video_async(frame).unwrap();
        sender.send_video(&test_owned_frame(9)).unwrap();
        sender.flush_async_blocking();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(sender.finish_video_async().unwrap().data().as_ptr(), pixels);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A copied send recovers the previous owned frame after synchronization.
        sender.send_video_async(test_owned_frame(2)).unwrap();
        let data = vec![3; 1024];
        let metadata = CString::new("<copy/>").unwrap();
        {
            let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
                .metadata(&metadata)
                .build()
                .unwrap();
            assert_eq!(
                sender
                    .send_video_async_copy(&borrowed)
                    .unwrap()
                    .unwrap()
                    .data()[0],
                2
            );
        }
        {
            let video = sender.inner.video.lock().unwrap();
            let Some(copy) = video.frame() else {
                panic!("missing copied snapshot")
            };
            assert_ne!(copy.data().as_ptr(), data.as_ptr());
            assert_ne!(copy.metadata_cstr().unwrap().as_ptr(), metadata.as_ptr());
        }
        drop(data);
        drop(metadata);
        {
            let video = sender.inner.video.lock().unwrap();
            let Some(copy) = video.frame() else {
                panic!("missing copied snapshot")
            };
            assert_eq!(copy.data(), vec![3; 1024]);
            assert_eq!(copy.metadata().unwrap(), "<copy/>");
        }
        // Copies are ordinary owned frames and are returned for reuse too.
        let copied = sender
            .send_video_async(test_owned_frame(4))
            .unwrap()
            .unwrap();
        assert_eq!(copied.data()[0], 3);
        assert_eq!(copied.metadata(), Some("<copy/>"));
        sender.finish_video_async().unwrap();
        sender.flush_async_blocking();
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn every_synchronizing_call_completes_a_frame_once_and_recovery_keeps_its_identity() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let notified = || count.load(Ordering::SeqCst);
        let in_flight = |sender: &Sender| sender.inner.video.lock().unwrap().is_in_flight();
        let frame = |value| {
            let frame = test_owned_frame(value);
            let pixels = frame.data().as_ptr();
            (frame, pixels)
        };

        // A synchronous send completes the frame in flight; the next
        // submission returns that allocation without notifying it again.
        let (a, a_pixels) = frame(1);
        assert!(sender.send_video_async(a).unwrap().is_none());
        assert!(in_flight(&sender));
        assert_eq!(notified(), 0);
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert!(!in_flight(&sender));
        assert_eq!(notified(), 1);
        let (b, b_pixels) = frame(2);
        let previous = sender.send_video_async(b).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), a_pixels);
        assert_eq!(notified(), 1);

        // A flush does the same, and a second flush finds nothing in flight.
        sender.flush_async_blocking();
        assert!(!in_flight(&sender));
        sender.flush_async_blocking();
        assert_eq!(notified(), 2);
        let (c, c_pixels) = frame(3);
        let previous = sender.send_video_async(c).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), b_pixels);
        assert_eq!(notified(), 2);

        // A submission completes the in-flight frame it displaces.
        let (d, d_pixels) = frame(4);
        let previous = sender.send_video_async(d).unwrap().unwrap();
        assert_eq!(previous.data().as_ptr(), c_pixels);
        assert!(in_flight(&sender));
        assert_eq!(notified(), 3);

        // Finishing recovers a frame a synchronous send already completed
        // without notifying it again.
        sender.send_video(&test_owned_frame(9)).unwrap();
        assert_eq!(notified(), 4);
        assert_eq!(
            sender.finish_video_async().unwrap().data().as_ptr(),
            d_pixels
        );
        assert!(sender.finish_video_async().is_none());
        assert_eq!(notified(), 4);

        // Destroying a sender whose retained frame was already completed
        // notifies nothing more.
        sender.send_video_async(frame(5).0).unwrap();
        sender.flush_async_blocking();
        assert_eq!(notified(), 5);
        drop(sender);
        assert_eq!(notified(), 5);
    }

    #[test]
    fn callback_panic_after_submission_keeps_new_owned_frame_recoverable() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("intentional completion panic");
            }
        });
        sender.send_video_async(test_owned_frame(1)).unwrap();
        let second = test_owned_frame(2);
        let pixels = second.data().as_ptr();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sender.send_video_async(second);
        }));
        assert!(result.is_err());
        assert_eq!(sender.finish_video_async().unwrap().data().as_ptr(), pixels);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(sender.finish_video_async().is_none());
    }

    #[test]
    fn completion_callback_can_reenter_shared_flush_without_deadlock() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Weak,
        };
        let reference = Arc::new(Mutex::new(Weak::<Sender>::new()));
        let callback_reference = reference.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            let sender = callback_reference.lock().unwrap().upgrade().unwrap();
            sender.flush_async_blocking();
            sender.send_video(&test_owned_frame(7)).unwrap();
        });
        sender.send_video_async(test_owned_frame(5)).unwrap();
        let sender = Arc::new(sender);
        *reference.lock().unwrap() = Arc::downgrade(&sender);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| sender.flush_async_blocking());
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let mut sender = Arc::try_unwrap(sender).unwrap();
        assert_eq!(sender.finish_video_async().unwrap().data()[0], 5);
    }

    #[test]
    fn sender_drop_notifies_once_for_copy() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let data = vec![0; 1024];
        {
            let frame = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Bgra)
                .build()
                .unwrap();
            assert!(sender.send_video_async_copy(&frame).unwrap().is_none());
        }
        drop(data);
        drop(sender);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn completion_handler_can_be_replaced_and_cleared() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let Some(mut sender) = test_sender() else {
            return;
        };
        sender.on_async_video_done(|_| panic!("replaced handler must not run"));
        let observed = count.clone();
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        sender.send_video_async(test_owned_frame(1)).unwrap();
        sender.finish_video_async();
        sender.clear_async_video_done();
        sender.send_video_async(test_owned_frame(2)).unwrap();
        sender.finish_video_async();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_async_flush_uses_null_frame_pointer() {
        assert!(async_flush_frame_ptr().is_null());
    }

    #[test]
    fn planar_444_sends_check_the_senders_runtime_before_the_sdk() {
        let Some(mut sender) = test_sender() else {
            return;
        };
        let supported = sender
            .runtime()
            .require_feature(RuntimeFeature::Planar444Video)
            .is_ok();
        let frame = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::Nb4g)
            .build()
            .unwrap();
        let pixels = frame.data().as_ptr();
        let data = frame.data().to_vec();
        let borrowed = BorrowedVideoFrame::builder(&data, 16, 16, PixelFormat::Nb4g)
            .build()
            .unwrap();
        // An earlier frame stays in flight through every rejection.
        assert!(sender
            .send_video_async(test_owned_frame(1))
            .unwrap()
            .is_none());
        if supported {
            sender.send_video(&frame).unwrap();
            let previous = sender.send_video_async(frame).unwrap().unwrap();
            assert_eq!(previous.data()[0], 1);
            let previous = sender.send_video_async_copy(&borrowed).unwrap().unwrap();
            assert_eq!(previous.data().as_ptr(), pixels);
        } else {
            let unsupported = |error: &Error| {
                matches!(
                    error,
                    Error::UnsupportedCapability {
                        feature: RuntimeFeature::Planar444Video,
                        ..
                    }
                )
            };
            assert!(unsupported(&sender.send_video(&frame).unwrap_err()));
            let rejected = sender.send_video_async(frame).unwrap_err();
            assert!(unsupported(rejected.error()));
            assert_eq!(rejected.into_inner().data().as_ptr(), pixels);
            assert!(unsupported(
                &sender.send_video_async_copy(&borrowed).unwrap_err()
            ));
            assert!(sender.inner.video.lock().unwrap().is_in_flight());
        }
        assert!(sender.finish_video_async().is_some());
    }

    #[test]
    fn borrowed_builder_validates_padded_layouts_and_preserves_headers_without_allocating() {
        for format in [
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
            PixelFormat::Uyvy,
            PixelFormat::Uyva,
            PixelFormat::P216,
            PixelFormat::Pa16,
            PixelFormat::I420,
            PixelFormat::Yv12,
            PixelFormat::Nv12,
        ] {
            for stride in [
                0,
                format.line_stride(4).unwrap(),
                format.line_stride(4).unwrap() + 8,
            ] {
                let layout =
                    ValidatedVideoLayout::new_uncompressed(format, 4, 4, Some(stride)).unwrap();
                // A longer buffer is accepted; the frame is its layout's extent.
                let data = vec![19; layout.data_len_bytes + 3];
                let metadata = c"<pixels>é</pixels>";
                let rate = FrameRate::new(30000, 1001).unwrap();
                let (borrowed, allocations) = crate::test_alloc::allocation_count(|| {
                    let frame = BorrowedVideoFrame::builder(&data, 4, 4, format)
                        .line_stride(stride)
                        .frame_rate(rate)
                        .picture_aspect_ratio(1.25)
                        .scan_type(ScanType::Interlaced)
                        .timecode(Timecode::from_ticks(0))
                        .metadata(metadata)
                        .build()
                        .unwrap();
                    for _ in 0..3 {
                        assert_eq!(frame.metadata(), Some("<pixels>é</pixels>"));
                        assert_eq!(frame.data(), &data[..layout.data_len_bytes]);
                        assert_eq!(frame.pixel_format(), format);
                    }
                    frame
                });
                assert_eq!(allocations, 0);
                let (owned, allocations) =
                    crate::test_alloc::allocation_count(|| borrowed.to_owned());
                assert_eq!(allocations, 2);
                assert_eq!(owned.frame_rate(), rate);
                assert_eq!(owned.picture_aspect_ratio(), 1.25);
                assert_eq!(owned.scan_type(), ScanType::Interlaced);
                assert_eq!(
                    (owned.timecode(), owned.timestamp()),
                    (Timecode::from_ticks(0), None)
                );
                assert_eq!(owned.metadata(), borrowed.metadata());
                let (reborrowed, allocations) =
                    crate::test_alloc::allocation_count(|| BorrowedVideoFrame::from(&owned));
                assert_eq!(allocations, 0);
                assert_eq!(reborrowed.metadata(), borrowed.metadata());
                assert_eq!(reborrowed.frame_rate(), rate);
                assert!(BorrowedVideoFrame::builder(
                    &data[..layout.data_len_bytes - 1],
                    4,
                    4,
                    format
                )
                .line_stride(stride)
                .build()
                .is_err());
            }
            for stride in [1, i32::MAX.unsigned_abs() as usize, usize::MAX] {
                let expected = ValidatedVideoLayout::new_uncompressed(format, 4, 4, Some(stride));
                let result = BorrowedVideoFrame::builder(&[], 4, 4, format)
                    .line_stride(stride)
                    .build();
                assert!(expected.is_err());
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn borrowed_metadata_is_bounded_utf8_and_preserves_empty_presence() {
        let data = [0; 16];
        for metadata in [None, Some(c"")] {
            let mut builder = BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra);
            if let Some(metadata) = metadata {
                builder = builder.metadata(metadata);
            }
            let frame = builder.build().unwrap();
            let (owned, allocations) = crate::test_alloc::allocation_count(|| frame.to_owned());
            assert_eq!(allocations, 1 + usize::from(metadata.is_some()));
            assert_eq!(owned.metadata(), metadata.map(|_| ""));
        }
        let invalid = c"\xff";
        assert!(matches!(
            BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra)
                .metadata(invalid)
                .build(),
            Err(Error::InvalidUtf8(_))
        ));
        for len in [
            crate::frames::MAX_METADATA_BYTES,
            crate::frames::MAX_METADATA_BYTES + 1,
        ] {
            let mut bytes = vec![b'x'; len];
            bytes[len - 1] = 0;
            let metadata = CStr::from_bytes_with_nul(&bytes).unwrap();
            let result = BorrowedVideoFrame::builder(&data, 2, 2, PixelFormat::Bgra)
                .metadata(metadata)
                .build();
            assert_eq!(result.is_ok(), len == crate::frames::MAX_METADATA_BYTES);
        }
    }

    #[test]
    fn test_borrowed_frame_timecode_and_aspect_ratio() {
        let buffer = vec![0u8; 64 * 36 * 2];
        let frame = BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
            .build()
            .unwrap();
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(frame.picture_aspect_ratio(), 16.0 / 9.0);

        let frame = BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
            .picture_aspect_ratio(0.0)
            .build()
            .unwrap()
            .with_timecode(Timecode::from_ticks(1234));
        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
        assert_eq!(frame.picture_aspect_ratio(), 0.0);
        let raw = frame.to_raw();
        assert_eq!((raw.timecode, raw.picture_aspect_ratio), (1234, 0.0));
        assert_eq!(raw.timestamp, NDIlib_recv_timestamp_undefined);

        for bad in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(
                BorrowedVideoFrame::builder(&buffer, 64, 36, PixelFormat::Uyvy)
                    .picture_aspect_ratio(bad)
                    .build()
                    .is_err(),
                "{bad}"
            );
        }
    }

    /// Build a borrowed frame of `format` over `buffer` at 30 frames per second.
    fn borrowed(
        buffer: &[u8],
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<BorrowedVideoFrame<'_>> {
        BorrowedVideoFrame::builder(buffer, width, height, format).build()
    }

    #[test]
    fn test_borrowed_builder_exact_size() {
        // BGRA format: 1920x1080x4 bytes
        let buffer = vec![0u8; 1920 * 1080 * 4];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Bgra).is_ok());
    }

    #[test]
    fn test_borrowed_builder_oversized_buffer() {
        // Buffer larger than needed should succeed
        let buffer = vec![0u8; 1920 * 1080 * 4 + 1000];
        let frame = borrowed(&buffer, 1920, 1080, PixelFormat::Bgra).unwrap();
        assert_eq!(frame.data().len(), 1920 * 1080 * 4);
    }

    #[test]
    fn test_borrowed_builder_undersized_buffer() {
        // Buffer too small should fail
        let buffer = vec![0u8; 1920 * 1080 * 4 - 1];
        let result = borrowed(&buffer, 1920, 1080, PixelFormat::Bgra);
        assert!(result.is_err());
        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(msg.contains("Buffer too small"));
            assert!(msg.contains("Bgra"));
        }
    }

    #[test]
    fn test_borrowed_builder_uyvy() {
        // UYVY format: 1920x1080x2 bytes
        let buffer = vec![0u8; 1920 * 1080 * 2];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Uyvy).is_ok());
    }

    #[test]
    fn test_borrowed_builder_nv12() {
        // NV12 planar format: Y plane + UV plane
        let expected_size = PixelFormat::Nv12.buffer_len(1920, 1080).unwrap();
        let buffer = vec![0u8; expected_size];
        assert!(borrowed(&buffer, 1920, 1080, PixelFormat::Nv12).is_ok());
    }

    #[test]
    fn test_borrowed_builder_i420() {
        // I420 planar format
        let expected_size = PixelFormat::I420.buffer_len(640, 480).unwrap();
        let buffer = vec![0u8; expected_size];
        assert!(borrowed(&buffer, 640, 480, PixelFormat::I420).is_ok());
    }

    #[test]
    fn test_borrowed_builder_rejects_invalid_layout() {
        let buffer = vec![0u8; 4096];

        assert!(matches!(
            borrowed(&buffer, 0, 480, PixelFormat::Bgra),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            borrowed(&buffer, 640, 0, PixelFormat::Bgra),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            borrowed(&buffer, 641, 480, PixelFormat::I420),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_getters() {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let frame = BorrowedVideoFrame::builder(&buffer, 1920, 1080, PixelFormat::Bgra)
            .frame_rate(FrameRate::new(60, 1).unwrap())
            .build()
            .unwrap();

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), PixelFormat::Bgra);
        assert_eq!(frame.frame_rate(), FrameRate::new(60, 1).unwrap());
        assert_eq!(frame.picture_aspect_ratio(), 16.0 / 9.0);
        assert_eq!(frame.scan_type(), ScanType::Progressive);
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        assert_eq!(frame.data().len(), buffer.len());
        assert_eq!(frame.line_stride_bytes(), 1920 * 4);
        assert!(frame.metadata().is_none());
        // Copies share the borrow, and Debug summarizes rather than dumps pixels.
        let copy = frame;
        assert_eq!(copy.data().as_ptr(), frame.data().as_ptr());
        assert!(format!("{frame:?}").contains("data (bytes): 8294400"));
        assert_eq!(
            borrowed(&buffer, 1920, 1080, PixelFormat::Bgra)
                .unwrap()
                .frame_rate(),
            FrameRate::new(30, 1).unwrap()
        );
    }

    #[test]
    fn test_all_pixel_formats_validation() {
        // Test that validation works correctly for all pixel formats
        let test_cases = vec![
            (PixelFormat::Bgra, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Rgba, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Bgrx, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Rgbx, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Uyvy, 1920, 1080, 1920 * 1080 * 2),
            (PixelFormat::Uyva, 1920, 1080, 1920 * 1080 * 3),
            (PixelFormat::P216, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::Pa16, 1920, 1080, 1920 * 1080 * 6),
        ];

        for (format, width, height, expected_min_size) in test_cases {
            // Exact size should work
            let buffer = vec![0u8; expected_min_size];
            let result = borrowed(&buffer, width, height, format);
            assert!(result.is_ok(), "Failed for format {:?}", format);

            // One byte too small should fail
            if expected_min_size > 0 {
                let buffer = vec![0u8; expected_min_size - 1];
                let result = borrowed(&buffer, width, height, format);
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }

    #[test]
    fn test_planar_formats() {
        // Test planar 4:2:0 formats (NV12, I420, YV12)
        let width = 1920;
        let height = 1080;

        for format in [PixelFormat::Nv12, PixelFormat::I420, PixelFormat::Yv12] {
            let expected_size = format.buffer_len(width, height).unwrap();

            let buffer = vec![0u8; expected_size];
            let result = borrowed(&buffer, width, height, format);
            assert!(result.is_ok(), "Failed for planar format {:?}", format);

            // One byte too small should fail
            if expected_size > 0 {
                let buffer = vec![0u8; expected_size - 1];
                let result = borrowed(&buffer, width, height, format);
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }
}
