//! NDI sending functionality for video, audio, and metadata.

#[cfg(target_os = "windows")]
use std::sync::Mutex;
use std::{
    ffi::{CStr, CString},
    fmt,
    os::raw::{c_char, c_void},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

#[cfg(feature = "advanced_sdk")]
use crate::waitable_completion::WaitableCompletion;

use crate::{
    finder::Source,
    frames::{AudioFrame, LineStrideOrSize, MetadataFrame, PixelFormat, ScanType, VideoFrame},
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

/// Internal state that is reference-counted and shared between SendInstance and tokens
struct Inner {
    instance: NDIlib_send_instance_t,
    name: *mut c_char,
    groups: *mut c_char,
    async_state: AsyncState,
    destroyed: AtomicBool,
    callback_ptr: AtomicPtr<c_void>,
}

#[derive(Debug)]
pub struct Sender {
    inner: Arc<Inner>,
    _ndi: NDI,
}

type AsyncCallback = Box<dyn Fn(usize) + Send + Sync>;

/// Async completion state for video frames
struct AsyncState {
    video_buffer_ptr: AtomicPtr<u8>,
    video_buffer_len: AtomicUsize,
    video_callback: OnceLock<AsyncCallback>,

    #[cfg(feature = "advanced_sdk")]
    completion: WaitableCompletion,
}

impl fmt::Debug for AsyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("AsyncState");
        dbg.field("video_buffer_ptr", &self.video_buffer_ptr)
            .field("video_buffer_len", &self.video_buffer_len)
            .field("video_callback_set", &self.video_callback.get().is_some());

        #[cfg(feature = "advanced_sdk")]
        dbg.field("completed", &self.completion.is_complete());

        dbg.finish()
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance)
            .field("async_state", &self.async_state)
            .field("destroyed", &self.destroyed)
            .field("callback_ptr", &self.callback_ptr)
            .finish()
    }
}

impl Default for AsyncState {
    fn default() -> Self {
        Self {
            video_buffer_ptr: AtomicPtr::new(ptr::null_mut()),
            video_buffer_len: AtomicUsize::new(0),
            video_callback: OnceLock::new(),

            #[cfg(feature = "advanced_sdk")]
            completion: WaitableCompletion::new_completed(),
        }
    }
}

// SAFETY: All fields are thread-safe atomics or OnceLock
unsafe impl Send for AsyncState {}
unsafe impl Sync for AsyncState {}

// SAFETY: Inner contains NDI instance pointer which is thread-safe,
// and all other fields are thread-safe atomics or Send+Sync types
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

/// A borrowed video frame that references external pixel data.
/// Used for zero-copy async send operations.
///
/// Fields are private to enforce safety invariants - use `try_from_uncompressed`,
/// `try_from_compressed`, or `from_parts_unchecked` to construct.
pub struct BorrowedVideoFrame<'buf> {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) pixel_format: PixelFormat,
    pub(crate) frame_rate_n: i32,
    pub(crate) frame_rate_d: i32,
    pub(crate) picture_aspect_ratio: f32,
    pub(crate) scan_type: ScanType,
    pub(crate) timecode: i64,
    pub(crate) data: &'buf [u8],
    pub(crate) line_stride_or_size: LineStrideOrSize,
    pub(crate) metadata: Option<&'buf CStr>,
    pub(crate) timestamp: i64,
}

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Create a borrowed video frame from an uncompressed pixel buffer.
    ///
    /// This constructor validates that the buffer is large enough for the specified
    /// dimensions and pixel format, returning an error if validation fails.
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
        let stride = pixel_format.line_stride(width);
        let expected_len = pixel_format.info().buffer_len(stride, height);

        if data.len() < expected_len {
            return Err(Error::InvalidFrame(format!(
                "Buffer too small for format {pixel_format:?}: got {actual} bytes, expected at least {expected_len} bytes \
                 (width={width}, height={height}, stride={stride})",
                actual = data.len()
            )));
        }

        Ok(BorrowedVideoFrame {
            width,
            height,
            pixel_format,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: 0,
            data,
            line_stride_or_size: LineStrideOrSize::LineStrideBytes(stride),
            metadata: None,
            timestamp: 0,
        })
    }

    /// Create a borrowed video frame from a compressed payload.
    ///
    /// This constructor validates that the buffer is large enough for the specified
    /// data size, returning an error if validation fails.
    ///
    /// # Arguments
    ///
    /// * `data` - Borrowed slice containing compressed data
    /// * `data_size_bytes` - Size of compressed data in bytes
    /// * `width` - Frame width in pixels
    /// * `height` - Frame height in pixels
    /// * `pixel_format` - Compressed pixel format
    /// * `frame_rate_n` - Frame rate numerator
    /// * `frame_rate_d` - Frame rate denominator
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidFrame` if the buffer is too small for `data_size_bytes`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let compressed_data = vec![0u8; 100000]; // Compressed payload
    /// let frame = BorrowedVideoFrame::try_from_compressed(
    ///     &compressed_data,
    ///     100000,
    ///     1920,
    ///     1080,
    ///     PixelFormat::UYVY,
    ///     30,
    ///     1
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_from_compressed(
        data: &'buf [u8],
        data_size_bytes: i32,
        width: i32,
        height: i32,
        pixel_format: PixelFormat,
        frame_rate_n: i32,
        frame_rate_d: i32,
    ) -> Result<Self> {
        let expected_len = data_size_bytes as usize;

        if data.len() < expected_len {
            return Err(Error::InvalidFrame(format!(
                "Buffer too small for compressed format {pixel_format:?}: got {actual} bytes, expected at least {expected_len} bytes",
                actual = data.len()
            )));
        }

        Ok(BorrowedVideoFrame {
            width,
            height,
            pixel_format,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: 16.0 / 9.0,
            scan_type: ScanType::Progressive,
            timecode: 0,
            data,
            line_stride_or_size: LineStrideOrSize::DataSizeBytes(data_size_bytes),
            metadata: None,
            timestamp: 0,
        })
    }

    /// Create a borrowed video frame without validation (unsafe).
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - For uncompressed formats: `data.len() >= pixel_format.info().buffer_len(line_stride, height)`
    /// - For compressed formats: `data.len() >= data_size_bytes`
    /// - `width`, `height`, `frame_rate_n`, and `frame_rate_d` are valid (non-negative, non-zero where appropriate)
    /// - The stride/size in `line_stride_or_size` matches the actual data layout
    ///
    /// Violating these invariants will cause the NDI SDK to read out of bounds through FFI,
    /// leading to undefined behavior.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, LineStrideOrSize};
    /// let buffer = vec![0u8; 1920 * 1080 * 4];
    /// let stride = PixelFormat::BGRA.line_stride(1920);
    ///
    /// // SAFETY: Buffer is correctly sized for 1920x1080 BGRA
    /// let frame = unsafe {
    ///     BorrowedVideoFrame::from_parts_unchecked(
    ///         &buffer,
    ///         1920,
    ///         1080,
    ///         PixelFormat::BGRA,
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
        pixel_format: PixelFormat,
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
            pixel_format,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            scan_type,
            timecode,
            data,
            line_stride_or_size,
            metadata,
            timestamp,
        }
    }

    /// Get the frame width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Get the frame height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Get the pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
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

    /// Get the metadata, if any.
    pub fn metadata(&self) -> Option<&CStr> {
        self.metadata
    }

    /// Get the timestamp.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        // Validation is now performed at construction time, so no runtime checks needed here
        NDIlib_video_frame_v2_t {
            xres: self.width,
            yres: self.height,
            FourCC: self.pixel_format.into(),
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
        BorrowedVideoFrame {
            width: frame.width,
            height: frame.height,
            pixel_format: frame.pixel_format,
            frame_rate_n: frame.frame_rate_n,
            frame_rate_d: frame.frame_rate_d,
            picture_aspect_ratio: frame.picture_aspect_ratio,
            scan_type: frame.scan_type,
            timecode: frame.timecode,
            data: &frame.data,
            line_stride_or_size: frame.line_stride_or_size,
            metadata: frame.metadata.as_deref(),
            timestamp: frame.timestamp,
        }
    }
}

/// A token that tracks an async video send operation.
///
/// The token holds exclusive access to the sender and a borrow of the frame buffer,
/// ensuring memory safety at compile time. Only one async send can be in-flight
/// at a time in the non-advanced SDK build.
///
/// When the token is dropped, a flush is automatically performed to ensure the
/// NDI SDK releases the buffer before the token's borrows expire.
#[must_use = "AsyncVideoToken must be held to track the async operation"]
pub struct AsyncVideoToken<'a, 'buf> {
    // False positive: this field IS read in Drop impl (self.inner.destroyed, self.inner.instance, etc.)
    // The compiler doesn't track field access through references in Drop
    #[allow(dead_code)]
    inner: &'a Arc<Inner>,
    _buffer: &'buf [u8],
    _metadata: Option<&'buf CStr>,
}

impl Drop for AsyncVideoToken<'_, '_> {
    fn drop(&mut self) {
        #[cfg(not(feature = "advanced_sdk"))]
        {
            // Use compare_exchange to atomically check if Inner is being destroyed
            let not_destroyed = self
                .inner
                .destroyed
                .compare_exchange(false, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();

            if not_destroyed {
                let null_frame = NDIlib_video_frame_v2_t {
                    p_data: std::ptr::null_mut(),
                    xres: 0,
                    yres: 0,
                    FourCC: 0,
                    frame_rate_N: 0,
                    frame_rate_D: 0,
                    picture_aspect_ratio: 0.0,
                    frame_format_type: 0,
                    timecode: 0,
                    __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                        line_stride_in_bytes: 0,
                    },
                    p_metadata: std::ptr::null(),
                    timestamp: 0,
                };

                #[cfg(target_os = "windows")]
                {
                    // Use unwrap_or_else to handle poisoned mutex gracefully in Drop
                    let _lock = FLUSH_MUTEX
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    unsafe {
                        NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
                    }
                }

                #[cfg(not(target_os = "windows"))]
                unsafe {
                    NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
                }
            }

            if let Some(callback) = self.inner.async_state.video_callback.get() {
                callback(self._buffer.len());
            }
        }

        #[cfg(feature = "advanced_sdk")]
        {
            let timeout = Duration::from_secs(5);
            let _ = self
                .inner
                .async_state
                .completion
                .try_wait_timeout(timeout, "AsyncVideoToken");

            if let Some(callback) = self.inner.async_state.video_callback.get() {
                callback(self._buffer.len());
            }
        }
    }
}

impl<'a, 'buf> AsyncVideoToken<'a, 'buf> {
    /// Explicitly wait for the async video operation to complete.
    ///
    /// This method provides an explicit way to wait for completion instead of relying on `Drop`.
    /// It consumes the token, ensuring the buffer is safe to reuse after this call returns.
    ///
    /// # Behavior by SDK Version
    ///
    /// - **Standard SDK**: Sends a NULL frame to flush the pipeline, blocking until all pending
    ///   async video operations complete. This is the same behavior as dropping the token.
    /// - **Advanced SDK** (with `advanced_sdk` feature): Waits for the SDK completion callback to signal
    ///   that the buffer has been released, with a 5-second timeout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] if waiting for completion times out (advanced SDK only).
    /// With the standard SDK, this method always succeeds but may block.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, PixelFormat, BorrowedVideoFrame};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test Sender").build();
    /// let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(&buffer, 1920, 1080, PixelFormat::BGRA, 30, 1)?;
    /// let token = sender.send_video_async(&borrowed_frame);
    ///
    /// // Explicitly wait for completion instead of relying on Drop
    /// token.wait()?;
    ///
    /// // Now safe to reuse or drop the buffer
    /// buffer.clear();
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait(self) -> Result<()> {
        drop(self);
        Ok(())
    }

    /// Check if the async video operation has completed (advanced SDK only).
    ///
    /// This method is only available when the `advanced_sdk` feature is enabled, as it requires
    /// SDK completion callbacks to track the completion state.
    ///
    /// # Returns
    ///
    /// `true` if the NDI SDK has called the completion callback, indicating the buffer is no longer
    /// in use. `false` if the operation is still pending.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #[cfg(feature = "advanced_sdk")]
    /// # {
    /// # use grafton_ndi::{NDI, SenderOptions, PixelFormat, BorrowedVideoFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test Sender").build();
    /// let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(&buffer, 1920, 1080, PixelFormat::BGRA, 30, 1)?;
    /// let token = sender.send_video_async(&borrowed_frame);
    ///
    /// // Poll for completion
    /// while !token.is_complete() {
    ///     std::thread::sleep(std::time::Duration::from_millis(1));
    /// }
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    #[cfg(feature = "advanced_sdk")]
    pub fn is_complete(&self) -> bool {
        self.inner.async_state.completion.is_complete()
    }
}

impl Sender {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sender name is empty or contains only whitespace
    /// - Both `clock_video` and `clock_audio` are false (at least one must be true)
    /// - The sender name contains a null byte
    /// - The groups string contains a null byte
    /// - NDI fails to create the send instance
    pub fn new(ndi: &NDI, create_settings: &SenderOptions) -> Result<Self> {
        // Validate sender name
        if create_settings.name.trim().is_empty() {
            return Err(Error::InvalidConfiguration(
                "Sender name cannot be empty or contain only whitespace".into(),
            ));
        }

        // Validate that at least one clock is enabled
        if !create_settings.clock_video && !create_settings.clock_audio {
            return Err(Error::InvalidConfiguration(
                "At least one of clock_video or clock_audio must be true".into(),
            ));
        }

        let p_ndi_name =
            CString::new(create_settings.name.clone()).map_err(Error::InvalidCString)?;
        let p_groups = match &create_settings.groups {
            Some(groups) => CString::new(groups.clone())
                .map_err(Error::InvalidCString)?
                .into_raw(),
            None => ptr::null_mut(),
        };

        let p_ndi_name_raw = p_ndi_name.into_raw();
        let c_settings = NDIlib_send_create_t {
            p_ndi_name: p_ndi_name_raw,
            p_groups,
            clock_video: create_settings.clock_video,
            clock_audio: create_settings.clock_audio,
        };

        let instance = unsafe { NDIlib_send_create(&c_settings) };
        if instance.is_null() {
            unsafe {
                let _ = CString::from_raw(p_ndi_name_raw);
                if !p_groups.is_null() {
                    let _ = CString::from_raw(p_groups);
                }
            }
            Err(Error::InitializationFailed(
                "Failed to create NDI send instance".into(),
            ))
        } else {
            let inner = Arc::new(Inner {
                instance,
                name: p_ndi_name_raw,
                groups: p_groups,
                async_state: AsyncState::default(),
                destroyed: AtomicBool::new(false),
                callback_ptr: AtomicPtr::new(ptr::null_mut()),
            });

            #[cfg(feature = "advanced_sdk")]
            {
                // Store a non-owning pointer for the callback (no refcount increment)
                // SAFETY: The pointer remains valid as long as the Arc<Inner> exists,
                // which is guaranteed by our design: the callback is unregistered in Sender::drop
                // before the last Arc reference is dropped.
                let raw_inner = Arc::as_ptr(&inner) as *mut c_void;
                inner.callback_ptr.store(raw_inner, Ordering::Release);

                // Only called via FFI callback when has_async_completion_callback cfg is enabled
                #[allow(dead_code)]
                extern "C" fn video_done_cb(
                    opaque: *mut c_void,
                    frame: *const NDIlib_video_frame_v2_t,
                ) {
                    unsafe {
                        // SAFETY: opaque is a non-owning pointer to Inner, created via Arc::as_ptr.
                        // The pointer remains valid because:
                        // 1. The callback is unregistered in Sender::drop before Inner is destroyed
                        // 2. The Arc<Inner> is kept alive by the Sender that registered this callback
                        let inner: &Inner = &*(opaque as *const Inner);

                        let len = if !frame.is_null() {
                            #[allow(clippy::unnecessary_cast)]
                            let fourcc = PixelFormat::try_from((*frame).FourCC as u32)
                                .unwrap_or(PixelFormat::BGRA);

                            if fourcc.is_uncompressed() {
                                let line_stride = (*frame).__bindgen_anon_1.line_stride_in_bytes;
                                let height = (*frame).yres;
                                fourcc.info().buffer_len(line_stride, height)
                            } else {
                                (*frame).__bindgen_anon_1.data_size_in_bytes as usize
                            }
                        } else {
                            0
                        };

                        inner.async_state.completion.signal();

                        if let Some(cb) = inner.async_state.video_callback.get() {
                            (cb)(len);
                        }
                    }
                }

                #[cfg(has_async_completion_callback)]
                unsafe {
                    NDIlib_send_set_video_async_completion(
                        instance,
                        raw_inner,
                        Some(video_done_cb),
                    );
                }

                #[cfg(not(has_async_completion_callback))]
                {
                    inner.callback_ptr.store(ptr::null_mut(), Ordering::Release);
                }
            }

            Ok(Self {
                inner,
                _ndi: ndi.clone(),
            })
        }
    }

    /// Send a video frame **synchronously** (NDI copies the buffer immediately).
    pub fn send_video(&self, video_frame: &VideoFrame) {
        unsafe {
            NDIlib_send_send_video_v2(self.inner.instance, &video_frame.to_raw());
        }
    }

    /// Send a video frame asynchronously with zero-copy.
    ///
    /// Uses `NDIlib_send_send_video_async_v2` for zero-copy transmission.
    ///
    /// **IMPORTANT**: This method requires a mutable borrow of the sender, which
    /// enforces single-flight semantics at compile time. Only one async send can
    /// be in-flight at a time.
    ///
    /// Returns an `AsyncVideoToken` that holds borrows of both the sender and the
    /// frame buffer. The token must be kept alive until the frame has been transmitted.
    /// When the token is dropped, a flush is automatically performed to ensure the
    /// NDI SDK releases the buffer.
    ///
    /// # Type Safety
    ///
    /// The returned token holds:
    /// - A borrow of the sender (preventing multiple concurrent async sends)
    /// - A borrow of the frame buffer (preventing the buffer from being dropped)
    ///
    /// This ensures memory safety at compile time without runtime overhead.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, VideoFrame, BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send_options = SenderOptions::builder("MyCam")
    ///     .clock_video(true)
    ///     .clock_audio(true)
    ///     .build();
    /// let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;
    ///
    /// // Register callback to know when buffer is released
    /// sender.on_async_video_done(|len| println!("Buffer released: {len} bytes"));
    ///
    /// // Use borrowed buffer directly (zero-copy, no allocation)
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(&buffer, 1920, 1080, PixelFormat::BGRA, 30, 1)?;
    /// let token = sender.send_video_async(&borrowed_frame);
    ///
    /// // Buffer is owned by SDK until token is dropped
    /// drop(token); // This triggers automatic flush
    /// // Now safe to reuse buffer
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video_async<'b>(
        &'b mut self,
        video_frame: &BorrowedVideoFrame<'b>,
    ) -> AsyncVideoToken<'b, 'b> {
        #[cfg(feature = "advanced_sdk")]
        {
            self.inner.async_state.completion.reset();
        }

        unsafe {
            NDIlib_send_send_video_async_v2(self.inner.instance, &video_frame.to_raw());
        }

        AsyncVideoToken {
            inner: &self.inner,
            _buffer: video_frame.data,
            _metadata: video_frame.metadata,
        }
    }

    /// Sends an audio frame synchronously.
    ///
    /// This function copies the audio data immediately and returns, making the buffer
    /// available for reuse. The underlying NDI SDK function `NDIlib_send_send_audio_v3`
    /// performs a synchronous copy of the data.
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
    /// Returns an error if the metadata string contains a null byte.
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
    /// Returns an error if the metadata string contains a null byte.
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

    /// Register a handler that will be called once the SDK has released
    /// the last buffer passed to `send_video_async`.
    /// The callback receives the buffer length.
    ///
    /// **Note**: Due to the use of `OnceLock`, this callback can only be set once.
    /// Subsequent calls to this method will be silently ignored.
    pub fn on_async_video_done<F>(&self, handler: F)
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        let _ = self.inner.async_state.video_callback.set(Box::new(handler));
    }

    /// Flush pending async video operations synchronously.
    ///
    /// Sends a NULL video frame to the SDK which blocks until all pending
    /// async video operations are complete. This is necessary when using the
    /// standard SDK to ensure buffers are released before reuse.
    ///
    /// # Safety
    ///
    /// After this function returns, all previously sent async video buffers
    /// can be safely reused or freed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, BorrowedVideoFrame, PixelFormat};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test").build();
    /// let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let frame = BorrowedVideoFrame::try_from_uncompressed(&buffer, 1920, 1080, PixelFormat::BGRA, 30, 1)?;
    /// let token = sender.send_video_async(&frame);
    ///
    /// // Drop token to release the mutable borrow, then flush
    /// drop(token);
    /// sender.flush_async_blocking();
    ///
    /// // Buffer can now be safely reused
    /// buffer.fill(0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async_blocking(&self) {
        let null_frame = NDIlib_video_frame_v2_t {
            p_data: std::ptr::null_mut(),
            xres: 0,
            yres: 0,
            FourCC: 0,
            frame_rate_N: 0,
            frame_rate_D: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: 0,
            timecode: 0,
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: 0,
            },
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };

        #[cfg(target_os = "windows")]
        {
            // Use unwrap_or_else to handle poisoned mutex gracefully
            let _lock = FLUSH_MUTEX
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            unsafe {
                NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
            }
        }

        #[cfg(not(target_os = "windows"))]
        unsafe {
            NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
        }
    }

    /// Wait for pending async operations with timeout.
    ///
    /// With `advanced_sdk`, this waits up to the specified timeout for the
    /// in-flight frame's completion callback. Without `advanced_sdk`, this
    /// calls `flush_async_blocking` to drain pending operations.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the operation completed within the timeout
    /// - `Err(Error::Timeout)` if the timeout elapsed (advanced_sdk only)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let options = SenderOptions::builder("Test").build();
    /// let sender = grafton_ndi::Sender::new(&ndi, &options)?;
    ///
    /// // ... send some async frames ...
    ///
    /// // Wait with timeout for completion
    /// sender.flush_async(Duration::from_secs(1))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async(&self, timeout: Duration) -> Result<()> {
        #[cfg(feature = "advanced_sdk")]
        {
            self.inner
                .async_state
                .completion
                .wait_timeout(timeout)
                .map_err(Error::Timeout)
        }

        #[cfg(not(feature = "advanced_sdk"))]
        {
            let _ = timeout;
            self.flush_async_blocking();
            Ok(())
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return;
        }

        std::sync::atomic::fence(Ordering::SeqCst);

        unsafe {
            NDIlib_send_destroy(self.instance);
        }

        unsafe {
            if !self.name.is_null() {
                let _ = CString::from_raw(self.name);
            }
            if !self.groups.is_null() {
                let _ = CString::from_raw(self.groups);
            }
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        #[cfg(all(feature = "advanced_sdk", has_async_completion_callback))]
        {
            let callback_ptr = self.inner.callback_ptr.load(Ordering::Acquire);
            if !callback_ptr.is_null() {
                unsafe {
                    NDIlib_send_set_video_async_completion(
                        self.inner.instance,
                        ptr::null_mut(),
                        None,
                    );
                }

                let timeout = Duration::from_secs(5);
                let _ = self
                    .inner
                    .async_state
                    .completion
                    .try_wait_timeout(timeout, "Sender");

                self.inner
                    .callback_ptr
                    .store(ptr::null_mut(), Ordering::Release);
            }
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
/// The Advanced SDK provides `NDIlib_send_set_video_async_completion` for registering
/// buffer-release callbacks (not available in the standard SDK).
///
/// The `SendInstance` struct holds an opaque pointer and raw C string pointers
/// that are only freed in Drop, making it safe to move between threads.
///
/// Functions like `NDIlib_send_create` and `NDIlib_send_destroy` should be called
/// from a single thread.
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
/// Note: Creation and destruction (`NDIlib_send_create`/`NDIlib_send_destroy`)
/// are handled in our Rust wrapper to ensure single-threaded access.
unsafe impl Sync for Sender {}

#[derive(Debug)]
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

/// Builder for configuring `SendOptions` with ergonomic method chaining
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
        let expected_size = PixelFormat::NV12.buffer_size(width, height);
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
        let expected_size = PixelFormat::I420.buffer_size(width, height);
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
    fn test_try_from_compressed_exact_size() {
        let data_size = 100000;
        let buffer = vec![0u8; data_size];
        let result = BorrowedVideoFrame::try_from_compressed(
            &buffer,
            data_size as i32,
            1920,
            1080,
            PixelFormat::UYVY,
            30,
            1,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_try_from_compressed_undersized() {
        let data_size = 100000;
        let buffer = vec![0u8; data_size - 1];
        let result = BorrowedVideoFrame::try_from_compressed(
            &buffer,
            data_size as i32,
            1920,
            1080,
            PixelFormat::UYVY,
            30,
            1,
        );
        assert!(result.is_err());
        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(msg.contains("Buffer too small"));
        }
    }

    #[test]
    fn test_from_parts_unchecked() {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let stride = PixelFormat::BGRA.line_stride(1920);

        // SAFETY: Buffer is correctly sized for 1920x1080 BGRA
        let frame = unsafe {
            BorrowedVideoFrame::from_parts_unchecked(
                &buffer,
                1920,
                1080,
                PixelFormat::BGRA,
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
        assert_eq!(frame.pixel_format(), PixelFormat::BGRA);
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
        assert_eq!(frame.pixel_format(), PixelFormat::BGRA);
        assert_eq!(frame.frame_rate_n(), 60);
        assert_eq!(frame.frame_rate_d(), 1);
        assert_eq!(frame.picture_aspect_ratio(), 16.0 / 9.0);
        assert_eq!(frame.scan_type(), ScanType::Progressive);
        assert_eq!(frame.timecode(), 0);
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
            (PixelFormat::PA16, 1920, 1080, 1920 * 1080 * 4),
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
            let expected_size = format.buffer_size(width, height);

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
