//! NDI sending functionality for video, audio, and metadata.

use crate::{
    error::Error,
    finder::Source,
    frames::{
        AudioFrame, FourCCVideoType, FrameFormatType, LineStrideOrSize, MetadataFrame, VideoFrame,
    },
    ndi_lib::*,
    receiver::{FrameType, Tally},
    NDI,
};
use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

// Compile-time check for atomic pointer support
#[cfg(not(target_has_atomic = "ptr"))]
compile_error!(
    "This crate requires atomic pointer support. Please use a target with atomics enabled."
);

/// Internal state that is reference-counted and shared between SendInstance and tokens
struct Inner {
    instance: NDIlib_send_instance_t,
    name: *mut c_char,   // Store raw pointer to free on drop
    groups: *mut c_char, // Store raw pointer to free on drop
    async_state: AsyncState,
    in_flight: AtomicUsize, // Count of outstanding async operations
    destroyed: AtomicBool,  // Flag to ensure drop runs only once
}

#[derive(Debug)]
pub struct SendInstance<'a> {
    inner: Arc<Inner>,
    ndi: std::marker::PhantomData<&'a NDI>,
}

/// Type alias for async completion callbacks  
/// The callback receives the buffer length, not the buffer itself
type AsyncCallback = Box<dyn Fn(usize) + Send + Sync>;

/// Lock-free async completion state
struct AsyncState {
    // Video async state (only video supports async in NDI SDK)
    video_buffer_ptr: AtomicPtr<u8>,
    video_buffer_len: AtomicUsize,
    video_callback: OnceLock<AsyncCallback>,
}

impl std::fmt::Debug for AsyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncState")
            .field("video_buffer_ptr", &self.video_buffer_ptr)
            .field("video_buffer_len", &self.video_buffer_len)
            .field("video_callback_set", &self.video_callback.get().is_some())
            .finish()
    }
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance)
            .field("async_state", &self.async_state)
            .field("in_flight", &self.in_flight)
            .field("destroyed", &self.destroyed)
            .finish()
    }
}

impl Default for AsyncState {
    fn default() -> Self {
        Self {
            video_buffer_ptr: AtomicPtr::new(ptr::null_mut()),
            video_buffer_len: AtomicUsize::new(0),
            video_callback: OnceLock::new(),
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
pub struct VideoFrameBorrowed<'buf> {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub data: &'buf [u8],
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<&'buf CStr>,
    pub timestamp: i64,
}

// AudioFrameBorrowed and MetadataFrameBorrowed removed - NDI SDK only supports async for video

impl<'buf> VideoFrameBorrowed<'buf> {
    /// Create a borrowed frame from a mutable buffer
    pub fn from_buffer(
        data: &'buf [u8],
        xres: i32,
        yres: i32,
        fourcc: FourCCVideoType,
        frame_rate_n: i32,
        frame_rate_d: i32,
    ) -> Self {
        let stride = match fourcc {
            FourCCVideoType::BGRA
            | FourCCVideoType::BGRX
            | FourCCVideoType::RGBA
            | FourCCVideoType::RGBX => xres * 4, // 32 bpp = 4 bytes per pixel
            FourCCVideoType::UYVY => xres * 2, // 16 bpp = 2 bytes per pixel
            FourCCVideoType::YV12 | FourCCVideoType::I420 | FourCCVideoType::NV12 => xres, // Y plane stride for planar formats
            FourCCVideoType::UYVA => xres * 3, // 24 bpp = 3 bytes per pixel
            FourCCVideoType::P216 | FourCCVideoType::PA16 => xres * 4, // 32 bpp = 4 bytes per pixel
            _ => xres * 4,                     // Default to 32 bpp
        };

        VideoFrameBorrowed {
            xres,
            yres,
            fourcc,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: FrameFormatType::Progressive,
            timecode: 0,
            data,
            line_stride_or_size: LineStrideOrSize {
                line_stride_in_bytes: stride,
            },
            metadata: None,
            timestamp: 0,
        }
    }

    fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.xres,
            yres: self.yres,
            FourCC: self.fourcc.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.frame_format_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.line_stride_or_size.into(),
            p_metadata: self.metadata.map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }
}

impl<'buf> From<&'buf VideoFrame<'_>> for VideoFrameBorrowed<'buf> {
    fn from(frame: &'buf VideoFrame<'_>) -> Self {
        VideoFrameBorrowed {
            xres: frame.xres,
            yres: frame.yres,
            fourcc: frame.fourcc,
            frame_rate_n: frame.frame_rate_n,
            frame_rate_d: frame.frame_rate_d,
            picture_aspect_ratio: frame.picture_aspect_ratio,
            frame_format_type: frame.frame_format_type,
            timecode: frame.timecode,
            data: &frame.data,
            line_stride_or_size: frame.line_stride_or_size,
            metadata: frame.metadata.as_deref(),
            timestamp: frame.timestamp,
        }
    }
}

/// A token that ensures the video frame remains valid while NDI is using it.
/// The frame will be released when this token is dropped or when the next
/// send operation occurs.
#[must_use = "AsyncVideoToken must be held until the next send operation"]
pub struct AsyncVideoToken<'buf> {
    inner: Arc<Inner>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
}

// Note: AsyncVideoToken implements Send because PhantomData<&'buf mut [u8]> is Send.
// This allows the token to be moved between threads, though the underlying buffer
// lifetime is still properly tracked.

impl Drop for AsyncVideoToken<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        eprintln!("AsyncVideoToken::drop - starting");

        // When token is dropped, trigger the completion callback
        trigger_video_completion(&self.inner);

        #[cfg(debug_assertions)]
        eprintln!("AsyncVideoToken::drop - completion triggered");

        // Decrement the in-flight counter
        let prev = self.inner.in_flight.fetch_sub(1, Ordering::Release);
        #[cfg(debug_assertions)]
        eprintln!(
            "AsyncVideoToken::drop - in_flight: {} -> {}",
            prev,
            prev - 1
        );
    }
}

// Audio and metadata tokens removed - NDI SDK only supports async for video

impl<'a> SendInstance<'a> {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sender name contains a null byte
    /// - The groups string contains a null byte
    /// - NDI fails to create the send instance
    pub fn new(_ndi: &'a NDI, create_settings: &SendOptions) -> Result<Self, Error> {
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
            // Clean up on error
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
            Ok(SendInstance {
                inner: Arc::new(Inner {
                    instance,
                    name: p_ndi_name_raw,
                    groups: p_groups,
                    async_state: AsyncState::default(),
                    in_flight: AtomicUsize::new(0),
                    destroyed: AtomicBool::new(false),
                }),
                ndi: std::marker::PhantomData,
            })
        }
    }

    /// Send a video frame **synchronously** (NDI copies the buffer immediately).
    pub fn send_video(&self, video_frame: &VideoFrame<'_>) {
        // Trigger any pending async completion callback before new send
        trigger_video_completion(&self.inner);

        unsafe {
            NDIlib_send_send_video_v2(self.inner.instance, &video_frame.to_raw());
        }
    }

    /// Send a video frame with lifetime management.
    ///
    /// **Note**: The NDI 6.1.1 SDK provides `NDIlib_send_set_video_async_completion`
    /// for true async callbacks, but our current bindings don't expose this API yet.
    /// As a workaround, this uses `NDIlib_send_send_video_v2` (synchronous copy) with
    /// a token system that fires callbacks on drop rather than when the SDK is done.
    ///
    /// Returns an `AsyncVideoToken` that must be held until safe to reuse the buffer.
    /// The frame data must remain valid as long as the token exists.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SendOptions, VideoFrame, VideoFrameBorrowed, FourCCVideoType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send_options = SendOptions::builder("MyCam")
    ///     .clock_video(true)
    ///     .clock_audio(true)
    ///     .build()?;
    /// let send = grafton_ndi::SendInstance::new(&ndi, &send_options)?;
    ///
    /// // Use borrowed buffer directly (zero-copy, no allocation)
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
    /// let _token = send.send_video_async(&borrowed_frame);
    /// // buffer is now being used by NDI - safe as long as token exists
    ///
    /// // When token is dropped or next send occurs, frame is released
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video_async<'b>(
        &'b self,
        video_frame: &VideoFrameBorrowed<'b>,
    ) -> AsyncVideoToken<'b> {
        // Trigger any pending async completion callback before new send
        trigger_video_completion(&self.inner);

        // Increment the in-flight counter
        let prev = self.inner.in_flight.fetch_add(1, Ordering::AcqRel);
        #[cfg(debug_assertions)]
        eprintln!("send_video_async - in_flight: {} -> {}", prev, prev + 1);

        // Store buffer info for async completion callback
        // SAFETY: We atomically store the buffer pointer and length.
        // The buffer is guaranteed to be valid as long as the AsyncVideoToken exists.
        self.inner
            .async_state
            .video_buffer_ptr
            .store(video_frame.data.as_ptr() as *mut u8, Ordering::Release);
        self.inner
            .async_state
            .video_buffer_len
            .store(video_frame.data.len(), Ordering::Release);

        // TODO: To implement true zero-copy async:
        // 1. Add bindings for NDIlib_send_set_video_async_completion
        // 2. Register a callback that triggers our completion handler
        // 3. Use NDIlib_send_send_video_async_v2 instead
        // 4. Fire user callbacks from the SDK thread, not from token Drop
        //
        // Currently using synchronous version for compatibility
        unsafe {
            NDIlib_send_send_video_v2(self.inner.instance, &video_frame.to_raw());
        }
        AsyncVideoToken {
            inner: self.inner.clone(),
            _frame: std::marker::PhantomData,
        }
    }

    // NOTE: Audio sending is always synchronous in NDI SDK.
    // There is no NDIlib_send_send_audio_async function.

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
    /// # use grafton_ndi::{NDI, SendOptions, AudioFrame};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let send = grafton_ndi::SendInstance::new(&ndi, &SendOptions::builder("Test").build()?)?;
    /// let mut audio_buffer = vec![0.0f32; 48000 * 2]; // 1 second of stereo audio
    ///
    /// // Fill buffer with audio data...
    /// let frame = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .channels(2)
    ///     .samples(48000)
    ///     .data(audio_buffer.clone())
    ///     .build()?;
    /// send.send_audio(&frame);
    ///
    /// // Buffer can be reused immediately
    /// audio_buffer.fill(0.5);
    /// let frame2 = AudioFrame::builder()
    ///     .sample_rate(48000)
    ///     .channels(2)
    ///     .samples(48000)
    ///     .data(audio_buffer)
    ///     .build()?;
    /// send.send_audio(&frame2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_audio(&self, audio_frame: &AudioFrame<'_>) {
        unsafe {
            NDIlib_send_send_audio_v3(self.inner.instance, &audio_frame.to_raw());
        }
    }

    // NOTE: Metadata sending is always synchronous in NDI SDK.
    // There is no NDIlib_send_send_metadata_async function.

    /// Sends a metadata frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata string contains a null byte.
    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe {
            NDIlib_send_send_metadata(self.inner.instance, &raw);
        }
        Ok(())
    }

    /// Captures a frame synchronously with timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Frame capture times out
    /// - Frame conversion fails
    /// - Frame data is invalid
    pub fn capture(&self, timeout_ms: u32) -> Result<FrameType<'static>, Error> {
        let mut metadata_frame = NDIlib_metadata_frame_t::default();
        let frame_type =
            unsafe { NDIlib_send_capture(self.inner.instance, &mut metadata_frame, timeout_ms) };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                if metadata_frame.p_data.is_null() {
                    Err(Error::NullPointer("Metadata frame data is null".into()))
                } else {
                    // Copy the metadata before it becomes invalid
                    let data = unsafe {
                        CStr::from_ptr(metadata_frame.p_data)
                            .to_string_lossy()
                            .into_owned()
                    };
                    let frame = MetadataFrame::with_data(data, metadata_frame.timecode);
                    Ok(FrameType::Metadata(frame))
                }
            }
            _ => Err(Error::CaptureFailed("Failed to capture frame".into())),
        }
    }

    // Note: free_metadata is no longer needed since MetadataFrame owns its data

    pub fn get_tally(&self, tally: &mut Tally, timeout_ms: u32) -> bool {
        unsafe { NDIlib_send_get_tally(self.inner.instance, &mut tally.to_raw(), timeout_ms) }
    }

    #[must_use]
    pub fn get_no_connections(&self, timeout_ms: u32) -> i32 {
        unsafe { NDIlib_send_get_no_connections(self.inner.instance, timeout_ms) }
    }

    pub fn clear_connection_metadata(&self) {
        unsafe { NDIlib_send_clear_connection_metadata(self.inner.instance) }
    }

    /// Adds connection metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata string contains a null byte.
    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe { NDIlib_send_add_connection_metadata(self.inner.instance, &raw) }
        Ok(())
    }

    /// Sets failover source.
    ///
    /// # Errors
    ///
    /// Returns an error if source conversion fails.
    pub fn set_failover(&self, source: &Source) -> Result<(), Error> {
        let raw_source = source.to_raw()?;
        unsafe { NDIlib_send_set_failover(self.inner.instance, &raw_source.raw) }
        Ok(())
    }

    #[must_use]
    pub fn get_source_name(&self) -> Source {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.inner.instance) };
        Source::from_raw(unsafe { &*source_ptr })
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

    // Audio and metadata async callbacks removed - NDI SDK only supports async for video

    /// Wait for pending async operations with timeout (optional helper).
    ///
    /// **Note**: The NDI SDK guarantees that `NDIlib_send_destroy` waits for all
    /// async operations to complete. This method is only useful if you want to
    /// explicitly wait with a timeout before dropping the SendInstance.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if all operations completed within the timeout
    /// - `Err(Error::Timeout)` if operations are still pending after the timeout
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, SendOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send = grafton_ndi::SendInstance::new(&ndi, &SendOptions::builder("Test").build()?)?;
    ///
    /// // ... send some async frames ...
    ///
    /// // Optional: wait with timeout before drop
    /// send.flush_async(Duration::from_secs(1))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async(&self, timeout: std::time::Duration) -> Result<(), Error> {
        let start = std::time::Instant::now();
        let mut spin_count = 0;
        let mut sleep_ms = 1;
        const MAX_SPIN: u32 = 100;
        const MAX_SLEEP_MS: u64 = 10;

        while self.inner.in_flight.load(Ordering::Acquire) > 0 {
            if start.elapsed() > timeout {
                return Err(Error::Timeout(format!(
                    "Async operations did not complete within {:?}",
                    timeout
                )));
            }

            if spin_count < MAX_SPIN {
                // First, spin for a bit
                std::thread::yield_now();
                spin_count += 1;
            } else {
                // Then sleep with exponential backoff
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                sleep_ms = (sleep_ms * 2).min(MAX_SLEEP_MS);
            }
        }

        Ok(())
    }
}

// Internal trigger functions that work with Inner
fn trigger_video_completion(inner: &Inner) {
    // Clear the buffer info
    let _ptr = inner
        .async_state
        .video_buffer_ptr
        .swap(ptr::null_mut(), Ordering::AcqRel);
    let len = inner.async_state.video_buffer_len.swap(0, Ordering::AcqRel);

    if len > 0 {
        if let Some(callback) = inner.async_state.video_callback.get() {
            // Just notify with the length, don't access the buffer
            callback(len);
        }
    }
}

// Audio and metadata trigger functions removed - NDI SDK only supports async for video

impl Drop for Inner {
    fn drop(&mut self) {
        // This is called when the last Arc reference is dropped
        // All tokens must be gone by this point
        #[cfg(debug_assertions)]
        eprintln!("Inner::drop - starting");

        // Ensure in_flight is 0
        let in_flight = self.in_flight.load(Ordering::Acquire);
        #[cfg(debug_assertions)]
        eprintln!("Inner::drop - in_flight: {}", in_flight);

        if in_flight > 0 {
            eprintln!(
                "WARNING: Inner::drop called with {} operations still in flight!",
                in_flight
            );
        }

        #[cfg(debug_assertions)]
        eprintln!("Inner::drop - triggering completion callbacks");

        #[cfg(debug_assertions)]
        eprintln!("Inner::drop - calling NDIlib_send_destroy");

        unsafe {
            // NDI SDK guarantees all async operations complete before this returns
            NDIlib_send_destroy(self.instance);

            #[cfg(debug_assertions)]
            eprintln!("Inner::drop - NDIlib_send_destroy completed");

            // Free the CStrings we allocated
            if !self.name.is_null() {
                #[cfg(debug_assertions)]
                eprintln!("Inner::drop - freeing name CString");
                let _ = CString::from_raw(self.name);
            }
            if !self.groups.is_null() {
                #[cfg(debug_assertions)]
                eprintln!("Inner::drop - freeing groups CString");
                let _ = CString::from_raw(self.groups);
            }
        }

        #[cfg(debug_assertions)]
        eprintln!("Inner::drop - completed");
    }
}

impl Drop for SendInstance<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        {
            eprintln!("SendInstance::drop - starting");
            eprintln!(
                "SendInstance::drop - Arc strong count: {}",
                Arc::strong_count(&self.inner)
            );
            eprintln!(
                "SendInstance::drop - in_flight: {}",
                self.inner.in_flight.load(Ordering::Acquire)
            );
        }
        // SendInstance drop doesn't need to do anything special
        // The Inner will be dropped when all Arc references are gone
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
/// The SDK also provides `NDIlib_send_set_video_async_completion` for registering
/// buffer-release callbacks, but our bindings don't expose this yet.
///
/// The `SendInstance` struct holds an opaque pointer and raw C string pointers
/// that are only freed in Drop, making it safe to move between threads.
///
/// Functions like `NDIlib_send_create` and `NDIlib_send_destroy` should be called
/// from a single thread.
unsafe impl std::marker::Send for SendInstance<'_> {}

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
unsafe impl std::marker::Sync for SendInstance<'_> {}

#[derive(Debug)]
pub struct SendOptions {
    pub name: String,
    pub groups: Option<String>,
    pub clock_video: bool,
    pub clock_audio: bool,
}

impl SendOptions {
    /// Create a builder for configuring send options
    pub fn builder<S: Into<String>>(name: S) -> SendOptionsBuilder {
        SendOptionsBuilder::new(name)
    }
}

/// Builder for configuring `SendOptions` with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct SendOptionsBuilder {
    name: String,
    groups: Option<String>,
    clock_video: Option<bool>,
    clock_audio: Option<bool>,
}

impl SendOptionsBuilder {
    /// Create a new builder with the specified name
    pub fn new<S: Into<String>>(name: S) -> Self {
        SendOptionsBuilder {
            name: name.into(),
            groups: None,
            clock_video: None,
            clock_audio: None,
        }
    }

    /// Set the groups for this sender
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

    /// Build the `SendOptions`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The name is empty or contains only whitespace
    /// - Both clock_video and clock_audio are false
    pub fn build(self) -> Result<SendOptions, Error> {
        // Validate sender name
        if self.name.trim().is_empty() {
            return Err(Error::InvalidConfiguration(
                "Sender name cannot be empty or contain only whitespace".into(),
            ));
        }

        let clock_video = self.clock_video.unwrap_or(true);
        let clock_audio = self.clock_audio.unwrap_or(true);

        // Validate that at least one clock is enabled
        if !clock_video && !clock_audio {
            return Err(Error::InvalidConfiguration(
                "At least one of clock_video or clock_audio must be true".into(),
            ));
        }

        Ok(SendOptions {
            name: self.name,
            groups: self.groups,
            clock_video,
            clock_audio,
        })
    }
}
