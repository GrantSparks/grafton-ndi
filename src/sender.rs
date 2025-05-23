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
};

#[derive(Debug)]
pub struct SendInstance<'a> {
    pub(crate) instance: NDIlib_send_instance_t,
    _name: *mut c_char,   // Store raw pointer to free on drop
    _groups: *mut c_char, // Store raw pointer to free on drop
    ndi: std::marker::PhantomData<&'a NDI>,
}

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
pub struct AsyncVideoToken<'send, 'buf> {
    _send: &'send SendInstance<'send>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
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
            })
        }
    }

    /// Send a video frame **synchronously** (NDI copies the buffer).
    pub fn send_video(&self, video_frame: &VideoFrame<'_>) {
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
        unsafe {
            NDIlib_send_send_video_async_v2(self.instance, &video_frame.to_raw());
        }
        AsyncVideoToken {
            _send: self,
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
}

impl Drop for SendInstance<'_> {
    fn drop(&mut self) {
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
