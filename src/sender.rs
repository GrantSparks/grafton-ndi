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
        atomic::{AtomicPtr, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

// Compile-time check for atomic pointer support
#[cfg(not(target_has_atomic = "ptr"))]
compile_error!(
    "This crate requires atomic pointer support. Please use a target with atomics enabled."
);

#[derive(Debug)]
pub struct SendInstance<'a> {
    pub(crate) instance: NDIlib_send_instance_t,
    _name: *mut c_char,   // Store raw pointer to free on drop
    _groups: *mut c_char, // Store raw pointer to free on drop
    ndi: std::marker::PhantomData<&'a NDI>,
    async_state: Arc<AsyncState>,
}

/// Type alias for async completion callbacks
type AsyncCallback = Box<dyn Fn(&mut [u8]) + Send + Sync>;

/// Lock-free async completion state
struct AsyncState {
    // Video async state
    video_buffer_ptr: AtomicPtr<u8>,
    video_buffer_len: AtomicUsize,
    video_callback: OnceLock<AsyncCallback>,

    // Audio async state (simulated)
    audio_buffer_ptr: AtomicPtr<u8>,
    audio_buffer_len: AtomicUsize,
    audio_callback: OnceLock<AsyncCallback>,

    // Metadata async state (simulated)
    metadata_buffer_ptr: AtomicPtr<u8>,
    metadata_buffer_len: AtomicUsize,
    metadata_callback: OnceLock<AsyncCallback>,
}

impl std::fmt::Debug for AsyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncState")
            .field("video_buffer_ptr", &self.video_buffer_ptr)
            .field("video_buffer_len", &self.video_buffer_len)
            .field("video_callback_set", &self.video_callback.get().is_some())
            .field("audio_buffer_ptr", &self.audio_buffer_ptr)
            .field("audio_buffer_len", &self.audio_buffer_len)
            .field("audio_callback_set", &self.audio_callback.get().is_some())
            .field("metadata_buffer_ptr", &self.metadata_buffer_ptr)
            .field("metadata_buffer_len", &self.metadata_buffer_len)
            .field(
                "metadata_callback_set",
                &self.metadata_callback.get().is_some(),
            )
            .finish()
    }
}

impl Default for AsyncState {
    fn default() -> Self {
        Self {
            video_buffer_ptr: AtomicPtr::new(ptr::null_mut()),
            video_buffer_len: AtomicUsize::new(0),
            video_callback: OnceLock::new(),
            audio_buffer_ptr: AtomicPtr::new(ptr::null_mut()),
            audio_buffer_len: AtomicUsize::new(0),
            audio_callback: OnceLock::new(),
            metadata_buffer_ptr: AtomicPtr::new(ptr::null_mut()),
            metadata_buffer_len: AtomicUsize::new(0),
            metadata_callback: OnceLock::new(),
        }
    }
}

// SAFETY: All fields are thread-safe atomics or OnceLock
unsafe impl Send for AsyncState {}
unsafe impl Sync for AsyncState {}

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

/// A borrowed audio frame that references external audio data.
/// Used for zero-copy async send operations.
pub struct AudioFrameBorrowed<'buf> {
    pub sample_rate: i32,
    pub no_channels: i32,
    pub no_samples: i32,
    pub timecode: i64,
    pub data: &'buf [u8],
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<&'buf CStr>,
    pub timestamp: i64,
}

impl<'buf> AudioFrameBorrowed<'buf> {
    /// Create a borrowed audio frame from a buffer of f32 samples
    pub fn from_buffer(
        data: &'buf [u8],
        sample_rate: i32,
        no_channels: i32,
        no_samples: i32,
    ) -> Self {
        AudioFrameBorrowed {
            sample_rate,
            no_channels,
            no_samples,
            timecode: 0,
            data,
            channel_stride_in_bytes: 0, // Interleaved
            metadata: None,
            timestamp: 0,
        }
    }
}

/// A borrowed metadata frame that references external metadata.
/// Used for zero-copy async send operations.
pub struct MetadataFrameBorrowed<'buf> {
    pub data: &'buf CStr,
    pub timecode: i64,
}

impl<'buf> MetadataFrameBorrowed<'buf> {
    /// Create a borrowed metadata frame from a CStr
    pub fn new(data: &'buf CStr) -> Self {
        MetadataFrameBorrowed { data, timecode: 0 }
    }
}

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
        let bpp = match fourcc {
            FourCCVideoType::BGRA
            | FourCCVideoType::BGRX
            | FourCCVideoType::RGBA
            | FourCCVideoType::RGBX => 32,
            FourCCVideoType::UYVY
            | FourCCVideoType::YV12
            | FourCCVideoType::I420
            | FourCCVideoType::NV12 => 16,
            FourCCVideoType::UYVA => 32,
            FourCCVideoType::P216 | FourCCVideoType::PA16 => 32,
            _ => 32,
        };
        let stride = (xres * bpp + 7) / 8;

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
#[repr(transparent)]
pub struct AsyncVideoToken<'send, 'buf> {
    sender: &'send SendInstance<'send>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
}

// Note: AsyncVideoToken does not implement Send, ensuring it stays on the producing thread

impl Drop for AsyncVideoToken<'_, '_> {
    fn drop(&mut self) {
        // When token is dropped, trigger the completion callback
        self.sender.trigger_video_completion();
    }
}

/// A token that ensures the audio frame remains valid while being processed.
/// The frame will be released when this token is dropped or when the next
/// send operation occurs.
#[must_use = "AsyncAudioToken must be held until the next send operation"]
#[repr(transparent)]
pub struct AsyncAudioToken<'send, 'buf> {
    sender: &'send SendInstance<'send>,
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
}

impl Drop for AsyncAudioToken<'_, '_> {
    fn drop(&mut self) {
        self.sender.trigger_audio_completion();
    }
}

/// A token that ensures the metadata frame remains valid while being processed.
/// The frame will be released when this token is dropped or when the next
/// send operation occurs.
#[must_use = "AsyncMetadataToken must be held until the next send operation"]
#[repr(transparent)]
pub struct AsyncMetadataToken<'send, 'buf> {
    sender: &'send SendInstance<'send>,
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
}

impl Drop for AsyncMetadataToken<'_, '_> {
    fn drop(&mut self) {
        self.sender.trigger_metadata_completion();
    }
}

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
                instance,
                _name: p_ndi_name_raw,
                _groups: p_groups,
                ndi: std::marker::PhantomData,
                async_state: Arc::new(AsyncState::default()),
            })
        }
    }

    /// Send a video frame **synchronously** (NDI copies the buffer).
    pub fn send_video(&self, video_frame: &VideoFrame<'_>) {
        // Trigger any pending async completion callback before new send
        self.trigger_video_completion();

        unsafe {
            NDIlib_send_send_video_v2(self.instance, &video_frame.to_raw());
        }
    }

    /// Send a video frame **asynchronously** (NDI *keeps a pointer*; no copy).
    ///
    /// Returns an `AsyncVideoToken` that must be held until the next send operation.
    /// The frame data is guaranteed to remain valid as long as the token exists.
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
    ) -> AsyncVideoToken<'b, 'b> {
        // Trigger any pending async completion callback before new send
        self.trigger_video_completion();

        // Store buffer info for async completion callback
        // SAFETY: We atomically store the buffer pointer and length.
        // The buffer is guaranteed to be valid as long as the AsyncVideoToken exists.
        self.async_state
            .video_buffer_ptr
            .store(video_frame.data.as_ptr() as *mut u8, Ordering::Release);
        self.async_state
            .video_buffer_len
            .store(video_frame.data.len(), Ordering::Release);

        unsafe {
            NDIlib_send_send_video_async_v2(self.instance, &video_frame.to_raw());
        }
        AsyncVideoToken {
            sender: self,
            _frame: std::marker::PhantomData,
        }
    }

    /// Send an audio frame **asynchronously** (zero-copy).
    ///
    /// Since NDI SDK doesn't provide native async audio sending, this method
    /// simulates async behavior by immediately sending the frame but deferring
    /// the buffer release notification until the token is dropped.
    ///
    /// Returns an `AsyncAudioToken` that must be held until the audio data
    /// can be safely reused.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SendOptions, AudioFrameBorrowed};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send = grafton_ndi::SendInstance::new(&ndi, &SendOptions::builder("Test").build()?)?;
    ///
    /// let mut buffer = vec![0u8; 48000 * 2 * 4]; // 1 second stereo float32
    /// let borrowed_frame = AudioFrameBorrowed {
    ///     sample_rate: 48000,
    ///     no_channels: 2,
    ///     no_samples: 48000,
    ///     timecode: 0,
    ///     data: &buffer,
    ///     channel_stride_in_bytes: 0,
    ///     metadata: None,
    ///     timestamp: 0,
    /// };
    /// let _token = send.send_audio_async(&borrowed_frame);
    /// // buffer is now being used - safe as long as token exists
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_audio_async<'b>(
        &'b self,
        audio_frame: &AudioFrameBorrowed<'b>,
    ) -> AsyncAudioToken<'b, 'b> {
        // Trigger any pending async completion callback before new send
        self.trigger_audio_completion();

        // Store buffer info for async completion callback
        // SAFETY: We atomically store the buffer pointer and length.
        // The buffer is guaranteed to be valid as long as the AsyncAudioToken exists.
        self.async_state
            .audio_buffer_ptr
            .store(audio_frame.data.as_ptr() as *mut u8, Ordering::Release);
        self.async_state
            .audio_buffer_len
            .store(audio_frame.data.len(), Ordering::Release);

        // Convert to raw frame and send synchronously (NDI doesn't have async audio)
        let raw_frame = NDIlib_audio_frame_v3_t {
            sample_rate: audio_frame.sample_rate,
            no_channels: audio_frame.no_channels,
            no_samples: audio_frame.no_samples,
            timecode: audio_frame.timecode,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: audio_frame.data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: audio_frame.channel_stride_in_bytes,
            },
            p_metadata: audio_frame.metadata.map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: audio_frame.timestamp,
        };

        unsafe {
            NDIlib_send_send_audio_v3(self.instance, &raw_frame);
        }

        AsyncAudioToken {
            sender: self,
            _frame: std::marker::PhantomData,
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
            NDIlib_send_send_audio_v3(self.instance, &audio_frame.to_raw());
        }
    }

    /// Send a metadata frame **asynchronously** (zero-copy).
    ///
    /// Since NDI SDK doesn't provide native async metadata sending, this method
    /// simulates async behavior by immediately sending the frame but deferring
    /// the buffer release notification until the token is dropped.
    ///
    /// Returns an `AsyncMetadataToken` that must be held until the metadata
    /// can be safely reused.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SendOptions, MetadataFrameBorrowed};
    /// # use std::ffi::CString;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send = grafton_ndi::SendInstance::new(&ndi, &SendOptions::builder("Test").build()?)?;
    ///
    /// let metadata = CString::new("<xml>test</xml>").unwrap();
    /// let borrowed_frame = MetadataFrameBorrowed {
    ///     data: &metadata,
    ///     timecode: 0,
    /// };
    /// let _token = send.send_metadata_async(&borrowed_frame);
    /// // metadata is now being used - safe as long as token exists
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_metadata_async<'b>(
        &'b self,
        metadata_frame: &MetadataFrameBorrowed<'b>,
    ) -> AsyncMetadataToken<'b, 'b> {
        // Trigger any pending async completion callback before new send
        self.trigger_metadata_completion();

        let data_bytes = metadata_frame.data.to_bytes_with_nul();

        // Store buffer info for async completion callback
        // SAFETY: We atomically store the buffer pointer and length.
        // The buffer is guaranteed to be valid as long as the AsyncMetadataToken exists.
        self.async_state
            .metadata_buffer_ptr
            .store(data_bytes.as_ptr() as *mut u8, Ordering::Release);
        self.async_state
            .metadata_buffer_len
            .store(data_bytes.len(), Ordering::Release);

        // Convert to raw frame and send synchronously (NDI doesn't have async metadata)
        let raw_frame = NDIlib_metadata_frame_t {
            length: (data_bytes.len() - 1) as i32, // Exclude null terminator from length
            timecode: metadata_frame.timecode,
            p_data: metadata_frame.data.as_ptr() as *mut c_char,
        };

        unsafe {
            NDIlib_send_send_metadata(self.instance, &raw_frame);
        }

        AsyncMetadataToken {
            sender: self,
            _frame: std::marker::PhantomData,
        }
    }

    /// Sends a metadata frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata string contains a null byte.
    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe {
            NDIlib_send_send_metadata(self.instance, &raw);
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
            unsafe { NDIlib_send_capture(self.instance, &mut metadata_frame, timeout_ms) };

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
        unsafe { NDIlib_send_get_tally(self.instance, &mut tally.to_raw(), timeout_ms) }
    }

    #[must_use]
    pub fn get_no_connections(&self, timeout_ms: u32) -> i32 {
        unsafe { NDIlib_send_get_no_connections(self.instance, timeout_ms) }
    }

    pub fn clear_connection_metadata(&self) {
        unsafe { NDIlib_send_clear_connection_metadata(self.instance) }
    }

    /// Adds connection metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata string contains a null byte.
    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe { NDIlib_send_add_connection_metadata(self.instance, &raw) }
        Ok(())
    }

    /// Sets failover source.
    ///
    /// # Errors
    ///
    /// Returns an error if source conversion fails.
    pub fn set_failover(&self, source: &Source) -> Result<(), Error> {
        let raw_source = source.to_raw()?;
        unsafe { NDIlib_send_set_failover(self.instance, &raw_source.raw) }
        Ok(())
    }

    #[must_use]
    pub fn get_source_name(&self) -> Source {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.instance) };
        Source::from_raw(unsafe { &*source_ptr })
    }

    /// Register a handler that will be called once the SDK has released
    /// the last buffer passed to `send_video_async`.
    /// The slice is **only valid for the duration of the callback**.
    pub fn on_async_video_done<F>(&self, handler: F)
    where
        F: Fn(&mut [u8]) + Send + Sync + 'static,
    {
        let _ = self.async_state.video_callback.set(Box::new(handler));
    }

    /// Register a handler for async audio completion.
    /// The slice is **only valid for the duration of the callback**.
    pub fn on_async_audio_done<F>(&self, handler: F)
    where
        F: Fn(&mut [u8]) + Send + Sync + 'static,
    {
        let _ = self.async_state.audio_callback.set(Box::new(handler));
    }

    /// Register a handler for async metadata completion.
    /// The slice is **only valid for the duration of the callback**.
    pub fn on_async_metadata_done<F>(&self, handler: F)
    where
        F: Fn(&mut [u8]) + Send + Sync + 'static,
    {
        let _ = self.async_state.metadata_callback.set(Box::new(handler));
    }

    /// Internal method to trigger video async completion callback
    fn trigger_video_completion(&self) {
        // Atomically swap out the buffer pointer with null
        // SAFETY: We atomically swap the pointer, ensuring it's only accessed once
        let ptr = self
            .async_state
            .video_buffer_ptr
            .swap(ptr::null_mut(), Ordering::AcqRel);
        let len = self.async_state.video_buffer_len.swap(0, Ordering::AcqRel);

        if !ptr.is_null() && len > 0 {
            if let Some(callback) = self.async_state.video_callback.get() {
                // SAFETY: The buffer was valid when async send was called,
                // and we've atomically taken ownership of the pointer
                let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
                callback(slice);
            }
        }
    }

    /// Internal method to trigger audio async completion callback
    fn trigger_audio_completion(&self) {
        // Atomically swap out the buffer pointer with null
        // SAFETY: We atomically swap the pointer, ensuring it's only accessed once
        let ptr = self
            .async_state
            .audio_buffer_ptr
            .swap(ptr::null_mut(), Ordering::AcqRel);
        let len = self.async_state.audio_buffer_len.swap(0, Ordering::AcqRel);

        if !ptr.is_null() && len > 0 {
            if let Some(callback) = self.async_state.audio_callback.get() {
                // SAFETY: The buffer was valid when async send was called,
                // and we've atomically taken ownership of the pointer
                let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
                callback(slice);
            }
        }
    }

    /// Internal method to trigger metadata async completion callback
    fn trigger_metadata_completion(&self) {
        // Atomically swap out the buffer pointer with null
        // SAFETY: We atomically swap the pointer, ensuring it's only accessed once
        let ptr = self
            .async_state
            .metadata_buffer_ptr
            .swap(ptr::null_mut(), Ordering::AcqRel);
        let len = self
            .async_state
            .metadata_buffer_len
            .swap(0, Ordering::AcqRel);

        if !ptr.is_null() && len > 0 {
            if let Some(callback) = self.async_state.metadata_callback.get() {
                // SAFETY: The buffer was valid when async send was called,
                // and we've atomically taken ownership of the pointer
                let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
                callback(slice);
            }
        }
    }
}

impl Drop for SendInstance<'_> {
    fn drop(&mut self) {
        // Trigger any pending async completion callbacks before destruction
        self.trigger_video_completion();
        self.trigger_audio_completion();
        self.trigger_metadata_completion();

        unsafe {
            NDIlib_send_destroy(self.instance);

            // Free the CStrings we allocated
            if !self._name.is_null() {
                let _ = CString::from_raw(self._name);
            }
            if !self._groups.is_null() {
                let _ = CString::from_raw(self._groups);
            }
        }
    }
}

/// # Safety
///
/// The NDI 6 SDK documentation states that send operations are thread-safe.
/// `NDIlib_send_send_video_v2`, `NDIlib_send_send_audio_v3`, and related functions
/// use internal synchronization. The `SendInstance` struct holds an opaque pointer and raw
/// C string pointers that are only freed in Drop, making it safe to move between threads.
unsafe impl std::marker::Send for SendInstance<'_> {}

/// # Safety
///
/// The NDI 6 SDK guarantees thread-safety for send operations. Multiple threads can
/// safely call send methods concurrently as the SDK handles all necessary synchronization.
/// The async send operations (`send_video_async`) are also thread-safe
/// as documented in the SDK manual.
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
    /// Returns an error if the name is empty.
    pub fn build(self) -> Result<SendOptions, Error> {
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
