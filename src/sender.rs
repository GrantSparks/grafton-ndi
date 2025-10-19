//! NDI sending functionality for video, audio, and metadata.

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    os::raw::{c_char, c_void},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
use std::sync::Mutex;

use crate::{
    finder::Source,
    frames::{
        calculate_line_stride, AudioFrame, FourCCVideoType, FrameFormatType, LineStrideOrSize,
        MetadataFrame, VideoFrame,
    },
    ndi_lib::*,
    receiver::Tally,
    Error, Result, NDI,
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
    name: *mut c_char,   // Store raw pointer to free on drop
    groups: *mut c_char, // Store raw pointer to free on drop
    async_state: AsyncState,
    in_flight: AtomicUsize,          // Count of outstanding async operations
    destroyed: AtomicBool,           // Flag to ensure drop runs only once
    callback_ptr: AtomicPtr<c_void>, // Store the raw pointer passed to NDI SDK
}

#[derive(Debug)]
pub struct Sender<'a> {
    inner: Arc<Inner>,
    ndi: PhantomData<&'a NDI>,
}

type AsyncCallback = Box<dyn Fn(usize) + Send + Sync>;

/// Lock-free async completion state
struct AsyncState {
    // Video async state (only video supports async in NDI SDK)
    video_buffer_ptr: AtomicPtr<u8>,
    video_buffer_len: AtomicUsize,
    video_callback: OnceLock<AsyncCallback>,
}

impl fmt::Debug for AsyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncState")
            .field("video_buffer_ptr", &self.video_buffer_ptr)
            .field("video_buffer_len", &self.video_buffer_len)
            .field("video_callback_set", &self.video_callback.get().is_some())
            .finish()
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("instance", &self.instance)
            .field("async_state", &self.async_state)
            .field("in_flight", &self.in_flight)
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
pub struct BorrowedVideoFrame<'buf> {
    pub width: i32,
    pub height: i32,
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

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Create a borrowed frame from a mutable buffer
    pub fn from_buffer(
        data: &'buf [u8],
        width: i32,
        height: i32,
        fourcc: FourCCVideoType,
        frame_rate_n: i32,
        frame_rate_d: i32,
    ) -> Self {
        let stride = calculate_line_stride(fourcc, width);

        BorrowedVideoFrame {
            width,
            height,
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
            xres: self.width,
            yres: self.height,
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

impl<'buf> From<&'buf VideoFrame> for BorrowedVideoFrame<'buf> {
    fn from(frame: &'buf VideoFrame) -> Self {
        BorrowedVideoFrame {
            width: frame.width,
            height: frame.height,
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

/// A token that tracks an async video send operation.
///
/// The token must be kept alive to track the operation. The actual buffer
/// release is handled by the SDK callback registered with `on_async_video_done`.
#[must_use = "AsyncVideoToken must be held to track the async operation"]
pub struct AsyncVideoToken<'buf> {
    inner: Arc<Inner>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: PhantomData<&'buf mut [u8]>,
}

// Note: AsyncVideoToken implements Send because PhantomData<&'buf mut [u8]> is Send.
// This allows the token to be moved between threads, though the underlying buffer
// lifetime is still properly tracked.

impl Drop for AsyncVideoToken<'_> {
    fn drop(&mut self) {
        // When using SDK callbacks, the callback handles notification
        #[cfg(not(feature = "advanced_sdk"))]
        {
            // Use stronger memory ordering for cross-platform consistency
            let prev_count = self.inner.in_flight.fetch_sub(1, Ordering::AcqRel);

            // If this was the last token, we need to flush to ensure the SDK releases the buffer
            if prev_count == 1 {
                // Use compare_exchange to atomically check if Inner is being destroyed
                // This prevents use-after-free race conditions
                let not_destroyed = self
                    .inner
                    .destroyed
                    .compare_exchange(
                        false,
                        false, // Don't actually change it, just check atomically
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok();

                if not_destroyed {
                    // Send NULL frame to flush per NDI docs
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

                    // On Windows, serialize flush operations to prevent deadlock
                    #[cfg(target_os = "windows")]
                    {
                        let _lock = FLUSH_MUTEX.lock().unwrap();
                        unsafe {
                            // This blocks until all pending async operations complete
                            NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
                        }
                    }

                    #[cfg(not(target_os = "windows"))]
                    unsafe {
                        // This blocks until all pending async operations complete
                        NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
                    }
                }
            }

            // Notify callback after flush
            if let Some(callback) = self.inner.async_state.video_callback.get() {
                // Notify with a dummy length since we don't have the actual buffer info
                callback(0);
            }
        }

        // When advanced_sdk is enabled, the SDK callback handles everything
        // But we still need to decrement the counter
        #[cfg(feature = "advanced_sdk")]
        {
            self.inner.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl<'a> Sender<'a> {
    /// Creates a new NDI send instance.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sender name contains a null byte
    /// - The groups string contains a null byte
    /// - NDI fails to create the send instance
    pub fn new(_ndi: &'a NDI, create_settings: &SenderOptions) -> Result<Self> {
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
            let inner = Arc::new(Inner {
                instance,
                name: p_ndi_name_raw,
                groups: p_groups,
                async_state: AsyncState::default(),
                in_flight: AtomicUsize::new(0),
                destroyed: AtomicBool::new(false),
                callback_ptr: AtomicPtr::new(ptr::null_mut()),
            });

            // Register SDK callback for async video completion if available
            // This requires NDI Advanced SDK 6.1.1+ which exports NDIlib_send_set_video_async_completion
            // NOTE: This function is only available in the Advanced SDK, not the standard SDK
            #[cfg(feature = "advanced_sdk")]
            {
                // Convert Arc to raw pointer for the callback
                // The Arc reference count is incremented here and will be decremented in Inner::drop
                let raw_inner = Arc::into_raw(inner.clone()) as *mut c_void;
                inner.callback_ptr.store(raw_inner, Ordering::Release);

                #[allow(dead_code)] // Will be used when NDIlib_send_set_video_async_completion is available
                extern "C" fn video_done_cb(
                    opaque: *mut c_void,
                    frame: *const NDIlib_video_frame_v2_t,
                ) {
                    unsafe {
                        // SAFETY: This pointer was created from Arc::into_raw and is still valid
                        // We clone the Arc here to access the Inner without consuming the original
                        let inner = Arc::from_raw(opaque as *const Inner);

                        // The frame has data_size_in_bytes for async frames
                        let len = if !frame.is_null() {
                            (*frame).__bindgen_anon_1.data_size_in_bytes as usize
                        } else {
                            0
                        };

                        // Call the user's completion callback if set
                        if let Some(cb) = inner.async_state.video_callback.get() {
                            (cb)(len);
                        }

                        // Decrement the in-flight counter
                        inner.in_flight.fetch_sub(1, Ordering::Release);

                        // Re-leak the Arc since we're not done with it yet
                        // It will be properly dropped in Inner::drop
                        ::std::mem::forget(inner);
                    }
                }

                // NOTE: Uncomment when NDIlib_send_set_video_async_completion is available in bindings
                /*
                unsafe {
                    NDIlib_send_set_video_async_completion(
                        instance,
                        raw_inner,
                        Some(video_done_cb),
                    );
                }
                */

                // For now, clean up the Arc since we can't register the callback
                let _ = unsafe { Arc::from_raw(raw_inner as *const Inner) };
                inner.callback_ptr.store(ptr::null_mut(), Ordering::Release);
            }

            Ok(Self {
                inner,
                ndi: PhantomData,
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
    /// **IMPORTANT**: The buffer remains owned by the SDK until a flush occurs.
    /// With the standard SDK, the library automatically flushes when the last
    /// AsyncVideoToken is dropped to ensure memory safety.
    ///
    /// Returns an `AsyncVideoToken` that must be held to track the operation.
    /// The frame data must remain valid until the token is dropped.
    ///
    /// # Safety
    ///
    /// The library ensures that when the last AsyncVideoToken is dropped, a flush
    /// is performed to release all buffers before the sender can be destroyed.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SenderOptions, VideoFrame, BorrowedVideoFrame, FourCCVideoType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send_options = SenderOptions::builder("MyCam")
    ///     .clock_video(true)
    ///     .clock_audio(true)
    ///     .build()?;
    /// let sender = grafton_ndi::Sender::new(&ndi, &send_options)?;
    ///
    /// // Register callback to know when buffer is released
    /// sender.on_async_video_done(|len| println!("Buffer released: {} bytes", len));
    ///
    /// // Use borrowed buffer directly (zero-copy, no allocation)
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
    /// let token = sender.send_video_async(&borrowed_frame);
    ///
    /// // Buffer is owned by SDK until token is dropped or explicit flush
    /// drop(token); // This triggers automatic flush if last token
    /// // Now safe to reuse buffer
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video_async<'b>(
        &'b self,
        video_frame: &BorrowedVideoFrame<'b>,
    ) -> AsyncVideoToken<'b> {
        // Increment the in-flight counter
        self.inner.in_flight.fetch_add(1, Ordering::AcqRel);

        // Use the real async send function
        unsafe {
            NDIlib_send_send_video_async_v2(self.inner.instance, &video_frame.to_raw());
        }

        AsyncVideoToken {
            inner: self.inner.clone(),
            _frame: PhantomData,
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
    /// # let sender = grafton_ndi::Sender::new(&ndi, &SenderOptions::builder("Test").build()?)?;
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
    /// # use grafton_ndi::{NDI, SenderOptions, BorrowedVideoFrame, FourCCVideoType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let sender = grafton_ndi::Sender::new(&ndi, &SenderOptions::builder("Test").build()?)?;
    ///
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let frame = BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
    /// let token = sender.send_video_async(&frame);
    ///
    /// // Flush to ensure buffer is released
    /// sender.flush_async_blocking();
    /// drop(token); // Now safe to drop token
    ///
    /// // Buffer can now be safely reused
    /// buffer.fill(0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async_blocking(&self) {
        // Send NULL frame per NDI docs to wait for all async operations
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

        // On Windows, serialize flush operations to prevent deadlock
        #[cfg(target_os = "windows")]
        {
            let _lock = FLUSH_MUTEX.lock().unwrap();
            unsafe {
                // This blocks until all pending async operations complete
                NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
            }
        }

        #[cfg(not(target_os = "windows"))]
        unsafe {
            // This blocks until all pending async operations complete
            NDIlib_send_send_video_async_v2(self.inner.instance, &null_frame);
        }
    }

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
    /// # use grafton_ndi::{NDI, SenderOptions};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let sender = grafton_ndi::Sender::new(&ndi, &SenderOptions::builder("Test").build()?)?;
    ///
    /// // ... send some async frames ...
    ///
    /// // Optional: wait with timeout before drop
    /// sender.flush_async(Duration::from_secs(1))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush_async(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();

        // Platform-specific flush behavior
        #[cfg(target_os = "windows")]
        {
            // On Windows, use more conservative synchronization
            std::sync::atomic::fence(Ordering::SeqCst);

            let mut sleep_ms = 1;
            const MAX_SLEEP_MS: u64 = 20; // Higher max sleep on Windows

            while self.inner.in_flight.load(Ordering::SeqCst) > 0 {
                if start.elapsed() > timeout {
                    return Err(Error::Timeout(format!(
                        "Async operations did not complete within {:?}",
                        timeout
                    )));
                }

                // On Windows, prefer sleeping over spinning to avoid tight loops
                thread::sleep(Duration::from_millis(sleep_ms));
                sleep_ms = (sleep_ms * 2).min(MAX_SLEEP_MS);
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Original implementation for other platforms
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
                    thread::yield_now();
                    spin_count += 1;
                } else {
                    // Then sleep with exponential backoff
                    thread::sleep(Duration::from_millis(sleep_ms));
                    sleep_ms = (sleep_ms * 2).min(MAX_SLEEP_MS);
                }
            }
        }

        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Prevent double-drop with maximum visibility
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return;
        }

        // Add a fence to ensure all previous operations are visible
        std::sync::atomic::fence(Ordering::SeqCst);

        // This is called when the last Arc reference is dropped
        // All tokens must be gone by this point

        // Check in_flight with SeqCst for maximum visibility
        let in_flight = self.in_flight.load(Ordering::SeqCst);

        if in_flight > 0 {
            eprintln!(
                "WARNING: Inner::drop called with {} operations still in flight!",
                in_flight
            );

            // On Windows, add a small delay to let any in-progress token drops complete
            // This prevents racing with AsyncVideoToken::drop
            #[cfg(target_os = "windows")]
            thread::sleep(Duration::from_millis(10));

            // Re-check after delay
            let in_flight_after = self.in_flight.load(Ordering::SeqCst);

            // Only flush if there are STILL operations in flight
            // This avoids double-flush with AsyncVideoToken::drop
            if in_flight_after > 0 {
                // Send NULL frame to flush per NDI docs
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

                // On Windows, serialize flush operations to prevent deadlock
                #[cfg(target_os = "windows")]
                {
                    let _lock = FLUSH_MUTEX.lock().unwrap();
                    unsafe {
                        // This blocks until all pending async operations complete
                        NDIlib_send_send_video_async_v2(self.instance, &null_frame);
                    }
                }

                #[cfg(not(target_os = "windows"))]
                unsafe {
                    // This blocks until all pending async operations complete
                    NDIlib_send_send_video_async_v2(self.instance, &null_frame);
                }
            }
        }

        // Now destroy the NDI instance
        unsafe {
            // NDI SDK guarantees all async operations complete before this returns
            NDIlib_send_destroy(self.instance);
        }

        // Then handle other cleanup
        unsafe {
            // Balance the Arc::into_raw from SendInstance::new when async callback is enabled
            // NOTE: This is only needed if the callback was actually registered
            #[cfg(feature = "advanced_sdk")]
            {
                let callback_ptr = self.callback_ptr.load(Ordering::Acquire);
                if !callback_ptr.is_null() {
                    // SAFETY: This pointer was created from Arc::into_raw in SendInstance::new
                    // In the current implementation where the SDK function is not available,
                    // this Arc was already cleaned up in SendInstance::new, so callback_ptr is null
                    let _ = Arc::from_raw(callback_ptr as *const Inner);
                }
            }

            // Free the CStrings we allocated
            // These must be freed after NDIlib_send_destroy to ensure the SDK is done with them
            if !self.name.is_null() {
                let _ = CString::from_raw(self.name);
            }
            if !self.groups.is_null() {
                let _ = CString::from_raw(self.groups);
            }
        }
    }
}

impl Drop for Sender<'_> {
    fn drop(&mut self) {
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
/// The Advanced SDK provides `NDIlib_send_set_video_async_completion` for registering
/// buffer-release callbacks (not available in the standard SDK).
///
/// The `SendInstance` struct holds an opaque pointer and raw C string pointers
/// that are only freed in Drop, making it safe to move between threads.
///
/// Functions like `NDIlib_send_create` and `NDIlib_send_destroy` should be called
/// from a single thread.
unsafe impl Send for Sender<'_> {}

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
unsafe impl Sync for Sender<'_> {}

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

    /// Build the `SendOptions`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The name is empty or contains only whitespace
    /// - Both clock_video and clock_audio are false
    pub fn build(self) -> Result<SenderOptions> {
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

        Ok(SenderOptions {
            name: self.name,
            groups: self.groups,
            clock_video,
            clock_audio,
        })
    }
}
