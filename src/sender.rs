//! NDI sending functionality for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt, ptr,
    sync::Mutex,
    time::Duration,
};

use crate::{
    finder::Source,
    frames::{
        AudioFrame, LineStrideOrSize, MetadataFrame, PixelFormat, ScanType, ValidatedVideoLayout,
        VideoFrame,
    },
    ndi_lib::*,
    receiver::Tally,
    to_ms_checked, Error, Result, NDI,
};

#[cfg(not(target_has_atomic = "ptr"))]
compile_error!(
    "This crate requires atomic pointer support. Please use a target with atomics enabled."
);

#[cfg(target_os = "windows")]
static FLUSH_MUTEX: Mutex<()> = Mutex::new(());

/// Flush the async video pipeline by passing a true NULL pointer.
///
/// The NDI SDK documentation specifies that calling
/// `NDIlib_send_send_video_async_v2(instance, NULL)` (where the *frame pointer*
/// is NULL, not merely a frame whose `p_data` is NULL) waits for any in-flight
/// async buffer to be released and then returns.
fn async_flush_frame_ptr() -> *const NDIlib_video_frame_v2_t {
    ptr::null()
}

fn flush_null_frame(instance: NDIlib_send_instance_t) {
    #[cfg(target_os = "windows")]
    {
        let _lock = FLUSH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            NDIlib_send_send_video_async_v2(instance, async_flush_frame_ptr());
        }
    }

    #[cfg(not(target_os = "windows"))]
    unsafe {
        NDIlib_send_send_video_async_v2(instance, async_flush_frame_ptr());
    }
}

/// SDK instance and the owned storage retained while it sends video.
struct Inner {
    instance: NDIlib_send_instance_t,
    _name: CString,
    _groups: Option<CString>,
    video_callback: Option<AsyncCallback>,
    // Dropped after Inner::drop drains the SDK.
    video: Mutex<VideoState>,
}

#[derive(Debug)]
pub struct Sender {
    inner: Inner,
    _ndi: NDI,
}

type AsyncCallback = Box<dyn Fn(usize) + Send + Sync>;

#[derive(Default)]
struct VideoState {
    pending: Option<VideoFrame>,
    in_flight: bool,
}

impl VideoState {
    // Claim notification before invoking user code so panic/reentry cannot
    // notify twice. The caller must first synchronize the SDK.
    fn completed(&mut self) -> Option<usize> {
        if std::mem::take(&mut self.in_flight) {
            self.pending.as_ref().map(|frame| frame.data().len())
        } else {
            None
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
            if video.in_flight {
                flush_null_frame(self.instance);
            }
            video.completed()
        };
        self.notify(len);
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance)
            .field("video_callback_set", &self.video_callback.is_some())
            .finish()
    }
}

// SAFETY: SDK access is thread-safe; video ownership and synchronization are
// serialized by the mutex. Other fields are owned strings and a Send + Sync callback.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

/// A borrowed video frame that references external pixel data.
/// [`Sender::send_video_async_copy`] copies its pixels and metadata into an
/// owned frame; source buffers can be reused immediately after submission.
///
/// Fields are private to enforce safety invariants. Use
/// [`Self::try_from_uncompressed`] for tightly packed typed formats or
/// [`Self::from_parts_unchecked`] for caller-validated raw descriptions. Owned
/// conversion and copied sending reject unsupported pixel formats.
pub struct BorrowedVideoFrame<'buf> {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) fourcc: u32,
    pub(crate) pixel_format: Option<PixelFormat>,
    pub(crate) frame_rate_n: i32,
    pub(crate) frame_rate_d: i32,
    pub(crate) picture_aspect_ratio: f32,
    pub(crate) scan_type: ScanType,
    pub(crate) timecode: i64,
    pub(crate) data: &'buf [u8],
    pub(crate) line_stride_or_size: LineStrideOrSize,
    pub(crate) layout: Option<ValidatedVideoLayout>,
    pub(crate) metadata: Option<&'buf CStr>,
    pub(crate) timestamp: i64,
}

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Create a borrowed video frame from an uncompressed pixel buffer.
    ///
    /// This constructor validates that the buffer is large enough for the specified
    /// dimensions and pixel format, returning an error if validation fails.
    /// Width must be positive and even for every pixel format. Height must be
    /// positive, and must also be even for 4:2:0 formats.
    ///
    /// # Arguments
    ///
    /// * `data` - Borrowed slice containing pixel data
    /// * `width` - Frame width in pixels
    /// * `height` - Frame height in pixels
    /// * `pixel_format` - Uncompressed pixel format (BGRA, UYVY, etc.)
    /// * `frame_rate_n` - Frame rate numerator (e.g., 60 for 60fps, 30000 for 29.97fps)
    /// * `frame_rate_d` - Frame rate denominator (e.g., 1 for 60fps, 1001 for 29.97fps)
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidFrame` if the buffer is too small for the specified format.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 4]; // BGRA buffer
    /// let frame = BorrowedVideoFrame::try_from_uncompressed(
    ///     &buffer,
    ///     1920,
    ///     1080,
    ///     PixelFormat::BGRA,
    ///     30,
    ///     1
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_from_uncompressed(
        data: &'buf [u8],
        width: i32,
        height: i32,
        pixel_format: PixelFormat,
        frame_rate_n: i32,
        frame_rate_d: i32,
    ) -> Result<Self> {
        crate::frames::validate_video_frame_metadata(frame_rate_n, frame_rate_d, 16.0 / 9.0)?;
        let layout = ValidatedVideoLayout::new_uncompressed(pixel_format, width, height, None)?;
        let expected_len = layout.data_len_bytes;

        if data.len() < expected_len {
            return Err(Error::InvalidFrame(format!(
                "Buffer too small for format {pixel_format:?}: got {actual} bytes, expected at least {expected_len} bytes \
                 (width={width}, height={height}, stride={stride:?})",
                stride = layout.line_stride_or_size,
                actual = data.len()
            )));
        }

        Ok(BorrowedVideoFrame {
            width,
            height,
            fourcc: pixel_format.into(),
            pixel_format: Some(pixel_format),
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: NDIlib_send_timecode_synthesize,
            data,
            line_stride_or_size: layout.line_stride_or_size,
            layout: Some(layout),
            metadata: None,
            timestamp: 0,
        })
    }

    /// Create a borrowed video frame without validation (unsafe).
    ///
    /// # Safety
    ///
    /// The caller must ensure all SDK-facing fields describe a valid frame:
    /// - `fourcc` is a correct NDI video FourCC for the payload.
    /// - The FourCC and `line_stride_or_size` union field are paired according
    ///   to the SDK contract: uncompressed formats use
    ///   [`LineStrideOrSize::LineStrideBytes`], compressed or opaque formats use
    ///   [`LineStrideOrSize::DataSizeBytes`].
    /// - Dimensions are positive where required by the SDK format, and any
    ///   planar format dimension or stride requirements are satisfied.
    /// - Line stride or data size is positive, fits the SDK field, and is
    ///   sufficient for every byte the SDK may read.
    /// - `data` is live and large enough for the final layout whenever the frame
    ///   is inspected or converted to owned storage.
    /// - `frame_rate_n` and `frame_rate_d` are positive, `picture_aspect_ratio`
    ///   is finite and nonnegative (`0.0` selects square pixels), and `scan_type`
    ///   matches a supported SDK scan type.
    /// - `metadata`, when present, remains valid, NUL-terminated, UTF-8, and
    ///   within the crate metadata size cap while the frame is inspected or copied.
    ///
    /// Conversion to an owned frame and copied async sending support only the
    /// crate's typed uncompressed layouts. Opaque or compressed FourCC payloads
    /// cannot be converted and return an error.
    ///
    /// This constructor passes `timecode` and `timestamp` through unchanged,
    /// including literal zero. Use `NDIlib_send_timecode_synthesize` to request
    /// SDK timecode synthesis, as the safe uncompressed constructor does by default.
    ///
    /// Violating these invariants can cause invalid reads while inspecting or
    /// copying the frame, leading to undefined behavior.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, LineStrideOrSize};
    /// let buffer = vec![0u8; 1920 * 1080 * 4];
    /// let stride = PixelFormat::BGRA.try_line_stride(1920).unwrap();
    ///
    /// // SAFETY: Buffer is correctly sized for 1920x1080 BGRA
    /// let frame = unsafe {
    ///     BorrowedVideoFrame::from_parts_unchecked(
    ///         &buffer,
    ///         1920,
    ///         1080,
    ///         PixelFormat::BGRA.into(),
    ///         30,
    ///         1,
    ///         16.0 / 9.0,
    ///         grafton_ndi::ScanType::Progressive,
    ///         0,
    ///         LineStrideOrSize::LineStrideBytes(stride),
    ///         None,
    ///         0,
    ///     )
    /// };
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn from_parts_unchecked(
        data: &'buf [u8],
        width: i32,
        height: i32,
        fourcc: u32,
        frame_rate_n: i32,
        frame_rate_d: i32,
        picture_aspect_ratio: f32,
        scan_type: ScanType,
        timecode: i64,
        line_stride_or_size: LineStrideOrSize,
        metadata: Option<&'buf CStr>,
        timestamp: i64,
    ) -> Self {
        BorrowedVideoFrame {
            width,
            height,
            fourcc,
            pixel_format: PixelFormat::try_from(fourcc).ok(),
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            scan_type,
            timecode,
            data,
            line_stride_or_size,
            layout: None,
            metadata,
            timestamp,
        }
    }

    /// Copy this borrowed frame into a validated owned frame.
    ///
    /// Pixels and metadata are copied; no reference to the source is retained.
    /// Only the supported uncompressed [`PixelFormat`] layouts are representable.
    /// Unsupported opaque or compressed FourCC values return an error.
    pub fn try_to_owned(&self) -> Result<VideoFrame> {
        // SAFETY: Safe constructors validate the slice and metadata, while the
        // unsafe constructor requires a valid SDK layout and live backing data.
        // from_raw validates supported layout details before copying the buffers.
        unsafe { VideoFrame::from_raw(&self.to_raw()) }
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Get the supported typed pixel format, if the raw FourCC is one of the
    /// crate's supported uncompressed formats.
    pub fn pixel_format(&self) -> Option<PixelFormat> {
        self.pixel_format
    }

    /// Get the raw SDK FourCC value.
    pub fn fourcc(&self) -> u32 {
        self.fourcc
    }

    /// Get the frame rate numerator.
    pub fn frame_rate_n(&self) -> i32 {
        self.frame_rate_n
    }

    /// Get the frame rate denominator.
    pub fn frame_rate_d(&self) -> i32 {
        self.frame_rate_d
    }

    /// Get the picture aspect ratio.
    pub fn picture_aspect_ratio(&self) -> f32 {
        self.picture_aspect_ratio
    }

    /// Get the scan type.
    pub fn scan_type(&self) -> ScanType {
        self.scan_type
    }

    /// Get the timecode.
    pub fn timecode(&self) -> i64 {
        self.timecode
    }

    /// Get a reference to the pixel data.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Get the line stride or data size.
    pub fn line_stride_or_size(&self) -> LineStrideOrSize {
        self.line_stride_or_size
    }

    /// Get the validated SDK data length for frames built through the safe
    /// typed constructor.
    ///
    /// Returns `None` for frames created with [`Self::from_parts_unchecked`],
    /// because those layouts are intentionally outside the crate's supported
    /// typed model.
    pub fn validated_data_len(&self) -> Option<usize> {
        self.layout.map(|layout| layout.data_len_bytes)
    }

    /// Get the metadata as UTF-8 text, if any.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.map(|metadata| {
            metadata
                .to_str()
                .expect("from_parts_unchecked requires UTF-8 metadata")
        })
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Set the timecode, in 100 ns units.
    ///
    /// [`try_from_uncompressed`](Self::try_from_uncompressed) leaves it at
    /// `NDIlib_send_timecode_synthesize`, which asks the SDK to make one up.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let buffer = vec![0u8; 1920 * 1080 * 2];
    /// let frame = BorrowedVideoFrame::try_from_uncompressed(
    ///     &buffer, 1920, 1080, PixelFormat::UYVY, 25, 1,
    /// )?
    /// .with_timecode(10_000_000);
    /// assert_eq!(frame.timecode(), 10_000_000);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_timecode(mut self, timecode: i64) -> Self {
        self.timecode = timecode;
        self
    }

    /// Set the picture aspect ratio, width over height as displayed; `0.0`
    /// means square pixels, whatever the resolution.
    ///
    /// [`try_from_uncompressed`](Self::try_from_uncompressed) assumes 16:9.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFrame`] if `ratio` is negative or not finite.
    pub fn with_picture_aspect_ratio(mut self, ratio: f32) -> Result<Self> {
        crate::frames::validate_video_frame_metadata(self.frame_rate_n, self.frame_rate_d, ratio)?;
        self.picture_aspect_ratio = ratio;
        Ok(self)
    }

    fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        // Validation is now performed at construction time, so no runtime checks needed here
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
            FourCC: self.fourcc as NDIlib_FourCC_video_type_e,
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.scan_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.line_stride_or_size.into(),
            p_metadata: self.metadata.map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }
}

impl<'buf> From<&'buf VideoFrame> for BorrowedVideoFrame<'buf> {
    fn from(frame: &'buf VideoFrame) -> Self {
        let layout = frame.validated_layout();
        BorrowedVideoFrame {
            width: frame.width(),
            height: frame.height(),
            fourcc: frame.pixel_format().into(),
            pixel_format: Some(frame.pixel_format()),
            frame_rate_n: frame.frame_rate_n(),
            frame_rate_d: frame.frame_rate_d(),
            picture_aspect_ratio: frame.picture_aspect_ratio(),
            scan_type: frame.scan_type(),
            timecode: frame.timecode(),
            data: frame.data(),
            line_stride_or_size: layout.line_stride_or_size,
            layout: Some(layout),
            metadata: frame.metadata_cstr(),
            timestamp: frame.timestamp(),
        }
    }
}

impl Sender {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sender name contains a null byte
    /// - The groups string contains a null byte
    /// - NDI fails to create the send instance
    pub fn new(ndi: &NDI, create_settings: &SenderOptions) -> Result<Self> {
        let name_cstr =
            CString::new(create_settings.name.clone()).map_err(Error::InvalidCString)?;
        let groups_cstr = match &create_settings.groups {
            Some(groups) => Some(CString::new(groups.clone()).map_err(Error::InvalidCString)?),
            None => None,
        };

        let c_settings = NDIlib_send_create_t {
            p_ndi_name: name_cstr.as_ptr(),
            p_groups: groups_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            clock_video: create_settings.clock_video,
            clock_audio: create_settings.clock_audio,
        };

        let instance = unsafe { NDIlib_send_create(&c_settings) };
        if instance.is_null() {
            Err(Error::InitializationFailed(
                "Failed to create NDI send instance".into(),
            ))
        } else {
            let inner = Inner {
                instance,
                _name: name_cstr,
                _groups: groups_cstr,
                video_callback: None,
                video: Mutex::new(VideoState::default()),
            };

            Ok(Self {
                inner,
                _ndi: ndi.clone(),
            })
        }
    }

    /// Send a video frame synchronously.
    ///
    /// The SDK has released the input buffer when this returns; the caller may
    /// then reuse or release it. This also completes a previous async video send.
    pub fn send_video(&self, video_frame: &VideoFrame) {
        let len = {
            let mut video = self.inner.video.lock().unwrap_or_else(|e| e.into_inner());
            // A synchronous video send also releases the previous async frame.
            unsafe {
                NDIlib_send_send_video_v2(self.inner.instance, &video_frame.to_raw());
            }
            video.completed()
        };
        self.inner.notify(len);
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
    pub fn send_video_async(&mut self, frame: VideoFrame) -> Option<VideoFrame> {
        let raw = frame.to_raw();
        let (previous, len) = {
            let mut video = self.inner.video.lock().unwrap_or_else(|e| e.into_inner());
            let len = video.completed();
            // Store the new owner before FFI, retaining the old owner locally
            // until this call synchronizes its SDK access. Moving a frame does
            // not move its Vec/CString allocations.
            let previous = video.pending.replace(frame);
            video.in_flight = true;
            unsafe {
                NDIlib_send_send_video_async_v2(self.inner.instance, &raw);
            }
            (previous, len)
        };
        // State is consistent and the mutex unlocked before user code. If it
        // panics, previous is safe to drop and the sender retains the new frame.
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
            if video.in_flight {
                flush_null_frame(self.inner.instance);
            }
            let len = video.completed();
            (video.pending.take(), len)
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
    /// Returns an error if the borrowed frame cannot be represented as a supported
    /// owned video layout. No pending send is changed when validation fails.
    pub fn send_video_async_copy(
        &mut self,
        frame: &BorrowedVideoFrame<'_>,
    ) -> Result<Option<VideoFrame>> {
        let owned = frame.try_to_owned()?;
        Ok(self.send_video_async(owned))
    }

    /// Sends an audio frame synchronously.
    ///
    /// The SDK has released the input buffer when this returns, so the caller
    /// may then reuse or release it. This calls `NDIlib_send_send_audio_v3`.
    ///
    /// See the NDI SDK documentation section on `NDIlib_send_send_audio_v3` for more details.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, AudioFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let options = SenderOptions::builder("Test").build();
    /// # let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let mut audio_buffer = vec![0.0f32; 48000 * 2]; // 1 second of stereo audio
    ///
    /// // Fill buffer with audio data...
    /// let frame = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .channels(2)
    ///     .samples(48000)
    ///     .data(audio_buffer.clone())
    ///     .build()?;
    /// sender.send_audio(&frame);
    ///
    /// // Buffer can be reused immediately
    /// audio_buffer.fill(0.5);
    /// let frame2 = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .channels(2)
    ///     .samples(48000)
    ///     .data(audio_buffer)
    ///     .build()?;
    /// sender.send_audio(&frame2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_audio(&self, audio_frame: &AudioFrame) {
        unsafe {
            NDIlib_send_send_audio_v3(self.inner.instance, &audio_frame.to_raw());
        }
    }

    /// Sends a metadata frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata contains an interior NUL byte, exceeds
    /// the metadata size limit, or cannot fit the SDK length field.
    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) -> Result<()> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe {
            NDIlib_send_send_metadata(self.inner.instance, &raw);
        }
        Ok(())
    }

    /// Get the current tally state for this sender.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for tally information.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// `Ok(Some(tally))` if tally was successfully retrieved, `Ok(None)` on timeout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test Sender").build();
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// // Try to get tally with 1 second timeout
    /// if let Some(tally) = sender.tally(Duration::from_secs(1))? {
    ///     println!("On program: {}, On preview: {}", tally.on_program, tally.on_preview);
    /// } else {
    ///     println!("Tally request timed out");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn tally(&self, timeout: Duration) -> Result<Option<Tally>> {
        let timeout_ms = to_ms_checked(timeout)?;
        let mut raw_tally = Tally::new(false, false).to_raw();
        let success =
            unsafe { NDIlib_send_get_tally(self.inner.instance, &mut raw_tally, timeout_ms) };

        if success {
            Ok(Some(Tally {
                on_program: raw_tally.on_program,
                on_preview: raw_tally.on_preview,
            }))
        } else {
            Ok(None)
        }
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
    /// Returns [`Error::Timeout`] if the SDK returns a negative value (indicating timeout or error).
    /// Returns [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test Sender").build();
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
        let count = unsafe { NDIlib_send_get_no_connections(self.inner.instance, timeout_ms) };

        if count < 0 {
            Err(Error::Timeout("Failed to obtain connection count".into()))
        } else {
            Ok(count as u32)
        }
    }

    pub fn clear_connection_metadata(&self) {
        unsafe { NDIlib_send_clear_connection_metadata(self.inner.instance) }
    }

    /// Adds connection metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata contains an interior NUL byte, exceeds
    /// the metadata size limit, or cannot fit the SDK length field.
    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) -> Result<()> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe { NDIlib_send_add_connection_metadata(self.inner.instance, &raw) }
        Ok(())
    }

    /// Sets failover source.
    ///
    /// # Errors
    ///
    /// Returns an error if source conversion fails.
    pub fn set_failover(&self, source: &Source) -> Result<()> {
        let raw_source = source.to_raw()?;
        unsafe { NDIlib_send_set_failover(self.inner.instance, &raw_source.raw) }
        Ok(())
    }

    /// Clear the failover source previously configured with [`Self::set_failover`].
    pub fn clear_failover(&self) {
        unsafe { NDIlib_send_set_failover(self.inner.instance, ptr::null()) }
    }

    /// Get the source information for this sender.
    ///
    /// # Errors
    ///
    /// Returns `Error::NullPointer` if the NDI SDK returns a null pointer or
    /// if the source data contains null pointers.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test Sender").build();
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    /// let source = sender.source()?;
    /// println!("Sender source: {source}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self) -> Result<Source> {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.inner.instance) };
        Source::try_from_raw(source_ptr)
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

impl Drop for Inner {
    fn drop(&mut self) {
        let video = self.video.get_mut().unwrap_or_else(|e| e.into_inner());
        if video.in_flight {
            flush_null_frame(self.instance);
        }
        let len = video.completed();
        // Destroy before running user code, so a callback panic cannot leak the
        // SDK instance. Pending allocations remain alive through destruction.
        unsafe { NDIlib_send_destroy(self.instance) };
        if std::thread::panicking() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.notify(len)));
        } else {
            self.notify(len);
        }
    }
}

/// # Safety
///
/// The NDI 6 SDK documentation specifically marks these send functions as thread-safe:
/// - `NDIlib_send_send_video_v2` and `NDIlib_send_send_video_async_v2`
/// - `NDIlib_send_send_audio_v3` (no async variant exists)
/// - `NDIlib_send_send_metadata` (no async variant exists)
/// - `NDIlib_send_get_tally`
/// - `NDIlib_send_get_no_connections`
///
/// `Inner` holds an opaque NDI pointer and owned CStrings that are automatically
/// freed in Drop, making it safe to move between threads.
///
/// Ownership prevents destruction while SDK calls still borrow the sender;
/// creation and destruction may occur on different threads.
unsafe impl Send for Sender {}

/// # Safety
///
/// The NDI 6 SDK guarantees that multiple threads can safely call send methods
/// concurrently. The SDK uses internal synchronization for:
/// - Video sending (both sync and async)
/// - Audio sending (sync only)
/// - Metadata sending
/// - Status queries (tally, connections)
///
/// Video ownership and synchronizing SDK calls are serialized by the mutex.
/// Exclusive ownership prevents concurrent instance destruction.
unsafe impl Sync for Sender {}

#[derive(Debug, Clone)]
pub struct SenderOptions {
    pub name: String,
    pub groups: Option<String>,
    pub clock_video: bool,
    pub clock_audio: bool,
}

impl SenderOptions {
    /// Create a builder for configuring send options
    pub fn builder<S: Into<String>>(name: S) -> SenderOptionsBuilder {
        SenderOptionsBuilder::new(name)
    }
}

/// Builder for configuring [`SenderOptions`] with method chaining.
#[derive(Debug, Clone)]
pub struct SenderOptionsBuilder {
    name: String,
    groups: Option<String>,
    clock_video: Option<bool>,
    clock_audio: Option<bool>,
}

impl SenderOptionsBuilder {
    /// Create a new builder with the specified name
    pub fn new<S: Into<String>>(name: S) -> Self {
        Self {
            name: name.into(),
            groups: None,
            clock_video: None,
            clock_audio: None,
        }
    }

    /// Set the groups for this sender
    #[must_use]
    pub fn groups<S: Into<String>>(mut self, groups: S) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Configure whether to clock video
    #[must_use]
    pub fn clock_video(mut self, clock: bool) -> Self {
        self.clock_video = Some(clock);
        self
    }

    /// Configure whether to clock audio
    #[must_use]
    pub fn clock_audio(mut self, clock: bool) -> Self {
        self.clock_audio = Some(clock);
        self
    }

    /// Build the sender options
    ///
    /// This method is infallible and simply applies defaults for any unset options.
    /// Validation is performed when creating a `Sender` via `Sender::new()`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, Sender};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("My Sender").build();
    /// let sender = Sender::new(&ndi, &options)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> SenderOptions {
        let clock_video = self.clock_video.unwrap_or(true);
        let clock_audio = self.clock_audio.unwrap_or(true);

        SenderOptions {
            name: self.name,
            groups: self.groups,
            clock_video,
            clock_audio,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_owned_frame(value: u8) -> VideoFrame {
        let mut frame = VideoFrame::builder()
            .resolution(16, 16)
            .pixel_format(PixelFormat::BGRA)
            .frame_rate(30, 1)
            .metadata(format!("<frame value='{value}'/>"))
            .build()
            .unwrap();
        frame.data_mut().fill(value);
        frame
    }

    fn test_sender() -> Sender {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "Owned async regression {} {}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let ndi = NDI::new().unwrap();
        Sender::new(
            &ndi,
            &SenderOptions::builder(name)
                .clock_video(false)
                .clock_audio(false)
                .build(),
        )
        .unwrap()
    }

    #[test]
    fn owned_async_recycles_original_pixel_and_metadata_allocations() {
        let mut sender = test_sender();
        let first = test_owned_frame(11);
        let pixels = first.data().as_ptr();
        let metadata = first.metadata_cstr().unwrap().as_ptr();
        assert!(sender.send_video_async(first).is_none());
        {
            let video = sender.inner.video.lock().unwrap();
            let Some(frame) = &video.pending else {
                panic!("missing owned frame")
            };
            assert_eq!(frame.to_raw().p_data, pixels as *mut u8);
            assert_eq!(frame.to_raw().p_metadata, metadata);
        }
        let second = test_owned_frame(22);
        let second_pixels = second.data().as_ptr();
        let recycled = sender.send_video_async(second).unwrap();
        assert_eq!(recycled.data().as_ptr(), pixels);
        assert_eq!(recycled.metadata_cstr().unwrap().as_ptr(), metadata);
        assert_eq!(recycled.metadata(), Some("<frame value='11'/>"));
        let final_frame = sender.finish_video_async().unwrap();
        assert_eq!(final_frame.data().as_ptr(), second_pixels);
        assert!(sender.finish_video_async().is_none());
    }

    #[test]
    fn mixed_sync_flush_and_copy_sends_notify_once_and_preserve_recovery() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let mut sender = test_sender();
        sender.on_async_video_done(move |len| {
            assert_eq!(len, 1024);
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let frame = test_owned_frame(1);
        let pixels = frame.data().as_ptr();
        sender.send_video_async(frame);
        sender.send_video(&test_owned_frame(9));
        sender.flush_async_blocking();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(sender.finish_video_async().unwrap().data().as_ptr(), pixels);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A copied send recovers the previous owned frame after synchronization.
        sender.send_video_async(test_owned_frame(2));
        let data = vec![3; 1024];
        let metadata = CString::new("<copy/>").unwrap();
        {
            let mut borrowed =
                BorrowedVideoFrame::try_from_uncompressed(&data, 16, 16, PixelFormat::BGRA, 30, 1)
                    .unwrap();
            borrowed.metadata = Some(&metadata);
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
            let Some(copy) = &video.pending else {
                panic!("missing copied snapshot")
            };
            assert_ne!(copy.data().as_ptr(), data.as_ptr());
            assert_ne!(copy.metadata_cstr().unwrap().as_ptr(), metadata.as_ptr());
        }
        drop(data);
        drop(metadata);
        {
            let video = sender.inner.video.lock().unwrap();
            let Some(copy) = &video.pending else {
                panic!("missing copied snapshot")
            };
            assert_eq!(copy.data(), vec![3; 1024]);
            assert_eq!(copy.metadata().unwrap(), "<copy/>");
        }
        // Copies are ordinary owned frames and are returned for reuse too.
        let copied = sender.send_video_async(test_owned_frame(4)).unwrap();
        assert_eq!(copied.data()[0], 3);
        assert_eq!(copied.metadata(), Some("<copy/>"));
        sender.finish_video_async().unwrap();
        sender.flush_async_blocking();
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn callback_panic_after_submission_keeps_new_owned_frame_recoverable() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let mut sender = test_sender();
        sender.on_async_video_done(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("intentional completion panic");
            }
        });
        sender.send_video_async(test_owned_frame(1));
        let second = test_owned_frame(2);
        let pixels = second.data().as_ptr();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sender.send_video_async(second);
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
        let mut sender = test_sender();
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            let sender = callback_reference.lock().unwrap().upgrade().unwrap();
            sender.flush_async_blocking();
            sender.send_video(&test_owned_frame(7));
        });
        sender.send_video_async(test_owned_frame(5));
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
        let mut sender = test_sender();
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let data = vec![0; 1024];
        {
            let frame =
                BorrowedVideoFrame::try_from_uncompressed(&data, 16, 16, PixelFormat::BGRA, 30, 1)
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
        let mut sender = test_sender();
        sender.on_async_video_done(|_| panic!("replaced handler must not run"));
        let observed = count.clone();
        sender.on_async_video_done(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        sender.send_video_async(test_owned_frame(1));
        sender.finish_video_async();
        sender.clear_async_video_done();
        sender.send_video_async(test_owned_frame(2));
        sender.finish_video_async();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_copy_leaves_pending_owned_frame_unchanged() {
        let mut sender = test_sender();
        let owned = test_owned_frame(7);
        let pixels = owned.data().as_ptr();
        sender.send_video_async(owned);
        let data = [0u8; 8];
        // SAFETY: This valid opaque payload is never submitted to the SDK;
        // copying rejects its unsupported FourCC before accessing its contents.
        let opaque = unsafe {
            BorrowedVideoFrame::from_parts_unchecked(
                &data,
                16,
                16,
                u32::from_le_bytes(*b"ABCD"),
                30,
                1,
                1.0,
                ScanType::Progressive,
                0,
                LineStrideOrSize::DataSizeBytes(8),
                None,
                0,
            )
        };
        assert!(sender.send_video_async_copy(&opaque).is_err());
        assert_eq!(sender.finish_video_async().unwrap().data().as_ptr(), pixels);
    }

    #[test]
    fn test_async_flush_uses_null_frame_pointer() {
        assert!(async_flush_frame_ptr().is_null());
    }

    #[test]
    fn test_borrowed_frame_setters() {
        let buffer = vec![0u8; 64 * 36 * 2];
        let frame =
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 64, 36, PixelFormat::UYVY, 25, 1)
                .unwrap();
        assert_eq!(frame.timecode(), NDIlib_send_timecode_synthesize);
        assert_eq!(frame.picture_aspect_ratio(), 16.0 / 9.0);

        let frame = frame
            .with_timecode(1234)
            .with_picture_aspect_ratio(0.0)
            .unwrap();
        assert_eq!(frame.timecode(), 1234);
        assert_eq!(frame.picture_aspect_ratio(), 0.0);
        let raw = frame.to_raw();
        assert_eq!((raw.timecode, raw.picture_aspect_ratio), (1234, 0.0));

        for bad in [-1.0, f32::NAN, f32::INFINITY] {
            let frame = BorrowedVideoFrame::try_from_uncompressed(
                &buffer,
                64,
                36,
                PixelFormat::UYVY,
                25,
                1,
            )
            .unwrap();
            assert!(frame.with_picture_aspect_ratio(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_try_from_uncompressed_exact_size() {
        // BGRA format: 1920x1080x4 bytes
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            1920,
            1080,
            PixelFormat::BGRA,
            30,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_uncompressed_oversized_buffer() {
        // Buffer larger than needed should succeed
        let buffer = vec![0u8; 1920 * 1080 * 4 + 1000];
        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            1920,
            1080,
            PixelFormat::BGRA,
            30,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_uncompressed_undersized_buffer() {
        // Buffer too small should fail
        let buffer = vec![0u8; 1920 * 1080 * 4 - 1];
        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            1920,
            1080,
            PixelFormat::BGRA,
            30,
            1,
        );
        assert!(result.is_err());
        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(msg.contains("Buffer too small"));
            assert!(msg.contains("BGRA"));
        }
    }

    #[test]
    fn test_try_from_uncompressed_uyvy() {
        // UYVY format: 1920x1080x2 bytes
        let expected_size = 1920 * 1080 * 2;
        let buffer = vec![0u8; expected_size];
        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            1920,
            1080,
            PixelFormat::UYVY,
            60,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_uncompressed_nv12() {
        // NV12 planar format: Y plane + UV plane
        let width = 1920;
        let height = 1080;
        let expected_size = PixelFormat::NV12.try_buffer_size(width, height).unwrap();
        let buffer = vec![0u8; expected_size];

        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            width,
            height,
            PixelFormat::NV12,
            30,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_uncompressed_i420() {
        // I420 planar format
        let width = 640;
        let height = 480;
        let expected_size = PixelFormat::I420.try_buffer_size(width, height).unwrap();
        let buffer = vec![0u8; expected_size];

        let result = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            width,
            height,
            PixelFormat::I420,
            30,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_uncompressed_rejects_invalid_layout() {
        let buffer = vec![0u8; 4096];

        assert!(matches!(
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 0, 480, PixelFormat::BGRA, 30, 1),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 640, -1, PixelFormat::BGRA, 30, 1),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 641, 480, PixelFormat::I420, 30, 1),
            Err(Error::InvalidFrame(_))
        ));
        assert!(matches!(
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 640, 480, PixelFormat::BGRA, 0, 1),
            Err(Error::InvalidFrame(_))
        ));
    }

    #[test]
    fn test_from_parts_unchecked() {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let stride = PixelFormat::BGRA.try_line_stride(1920).unwrap();

        // SAFETY: Buffer is correctly sized for 1920x1080 BGRA
        let frame = unsafe {
            BorrowedVideoFrame::from_parts_unchecked(
                &buffer,
                1920,
                1080,
                PixelFormat::BGRA.into(),
                30,
                1,
                16.0 / 9.0,
                ScanType::Progressive,
                0,
                LineStrideOrSize::LineStrideBytes(stride),
                None,
                0,
            )
        };

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), Some(PixelFormat::BGRA));
    }

    #[test]
    fn test_from_parts_unchecked_allows_raw_fourcc_escape_hatch() {
        let buffer = vec![0u8; 16];

        // SAFETY: This test does not send the frame to the SDK; it only verifies
        // that the unsafe escape hatch can carry an unsupported raw FourCC and
        // data-size union field without exposing it through a safe constructor.
        let frame = unsafe {
            BorrowedVideoFrame::from_parts_unchecked(
                &buffer,
                1,
                1,
                0x1234_5678,
                30,
                1,
                1.0,
                ScanType::Progressive,
                0,
                LineStrideOrSize::DataSizeBytes(buffer.len() as i32),
                None,
                0,
            )
        };

        assert_eq!(frame.fourcc(), 0x1234_5678);
        assert_eq!(frame.pixel_format(), None);
        assert_eq!(
            frame.line_stride_or_size(),
            LineStrideOrSize::DataSizeBytes(buffer.len() as i32)
        );
        assert_eq!(frame.validated_data_len(), None);
    }

    #[test]
    fn test_getters() {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let frame = BorrowedVideoFrame::try_from_uncompressed(
            &buffer,
            1920,
            1080,
            PixelFormat::BGRA,
            60,
            1,
        )
        .unwrap();

        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
        assert_eq!(frame.pixel_format(), Some(PixelFormat::BGRA));
        assert_eq!(frame.frame_rate_n(), 60);
        assert_eq!(frame.frame_rate_d(), 1);
        assert_eq!(frame.picture_aspect_ratio(), 16.0 / 9.0);
        assert_eq!(frame.scan_type(), ScanType::Progressive);
        assert_eq!(frame.timecode(), NDIlib_send_timecode_synthesize);
        assert_eq!(frame.data().len(), buffer.len());
        assert!(frame.metadata().is_none());
        assert_eq!(frame.timestamp(), 0);
    }

    #[test]
    fn test_all_pixel_formats_validation() {
        // Test that validation works correctly for all pixel formats
        let test_cases = vec![
            (PixelFormat::BGRA, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::RGBA, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::BGRX, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::RGBX, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::UYVY, 1920, 1080, 1920 * 1080 * 2),
            (PixelFormat::UYVA, 1920, 1080, 1920 * 1080 * 3),
            (PixelFormat::P216, 1920, 1080, 1920 * 1080 * 4),
            (PixelFormat::PA16, 1920, 1080, 1920 * 1080 * 6),
        ];

        for (format, width, height, expected_min_size) in test_cases {
            // Exact size should work
            let buffer = vec![0u8; expected_min_size];
            let result =
                BorrowedVideoFrame::try_from_uncompressed(&buffer, width, height, format, 30, 1);
            assert!(result.is_ok(), "Failed for format {:?}", format);

            // One byte too small should fail
            if expected_min_size > 0 {
                let buffer = vec![0u8; expected_min_size - 1];
                let result = BorrowedVideoFrame::try_from_uncompressed(
                    &buffer, width, height, format, 30, 1,
                );
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }

    #[test]
    fn test_planar_formats() {
        // Test planar 4:2:0 formats (NV12, I420, YV12)
        let width = 1920;
        let height = 1080;

        for format in [PixelFormat::NV12, PixelFormat::I420, PixelFormat::YV12] {
            let expected_size = format.try_buffer_size(width, height).unwrap();

            let buffer = vec![0u8; expected_size];
            let result =
                BorrowedVideoFrame::try_from_uncompressed(&buffer, width, height, format, 30, 1);
            assert!(result.is_ok(), "Failed for planar format {:?}", format);

            // One byte too small should fail
            if expected_size > 0 {
                let buffer = vec![0u8; expected_size - 1];
                let result = BorrowedVideoFrame::try_from_uncompressed(
                    &buffer, width, height, format, 30, 1,
                );
                assert!(result.is_err(), "Should fail for undersized {:?}", format);
            }
        }
    }
}
