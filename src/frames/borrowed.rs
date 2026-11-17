//! Validated application media whose private fields retain their checked
//! layouts, borrowed initialized storage and metadata together.

use super::*;

/// Validated planar floating-point audio borrowed from application storage.
///
/// Samples and optional NUL-terminated metadata are borrowed for the frame's
/// lifetime. Synchronous sending and [`crate::audio_conversion::AudioConverter`]
/// use the original channel stride without copying samples or allocating.
/// Inspect channels with [`Self::channel_data`] or [`Self::channels`]; those
/// slices exclude inter-channel padding.
#[derive(Clone, Copy)]
pub struct BorrowedAudioFrame<'buf> {
    layout: ValidatedAudioLayout,
    timecode: Timecode,
    data: &'buf [f32],
    metadata: Option<MetadataText<'buf>>,
}

// SAFETY: Private fields pair the f32 slice with its validated layout after
// the builder checks its initialized extent. Its borrow keeps the slice and validated terminated
// metadata alive and unchanged; the default descriptor uses only those parts.
unsafe impl AudioParts for BorrowedAudioFrame<'_> {
    fn layout(&self) -> &ValidatedAudioLayout {
        &self.layout
    }
    fn audio_timecode(&self) -> Timecode {
        self.timecode
    }
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        None
    }
    fn samples(&self) -> *const f32 {
        self.data.as_ptr()
    }
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        FrameMetadataRef::from_text(self.metadata)
    }
}

impl<'buf> BorrowedAudioFrame<'buf> {
    /// Configure a borrowed planar frame with explicit sample rate and shape.
    ///
    /// Defaults to tightly packed channels, [`Timecode::SYNTHESIZE`] and no
    /// metadata. A larger input slice is permitted; only the checked frame
    /// extent is retained. All validation occurs in the builder's `build`.
    ///
    /// ```
    /// use grafton_ndi::{AudioSource, BorrowedAudioFrame};
    /// let samples = [1.0, 2.0, 99.0, 3.0, 4.0];
    /// let frame = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
    ///     .channel_stride_bytes(12)
    ///     .build()?;
    /// let source: &dyn AudioSource = &frame;
    /// assert_eq!(source.channels().collect::<Vec<_>>(), [&[1.0, 2.0][..], &[3.0, 4.0][..]]);
    /// assert!(source.data().is_none());
    /// # Ok::<(), grafton_ndi::Error>(())
    /// ```
    pub fn builder(
        data: &'buf [f32],
        sample_rate: u32,
        num_channels: u32,
        num_samples: u32,
    ) -> BorrowedAudioFrameBuilder<'buf> {
        BorrowedAudioFrameBuilder {
            data,
            sample_rate,
            num_channels,
            num_samples,
            channel_stride_bytes: None,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
        }
    }

    /// Return this frame with the supplied timecode; no samples are copied.
    #[must_use]
    pub fn with_timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }
}

/// Builder for [`BorrowedAudioFrame`]; no sample or metadata copies are made.
#[derive(Clone, Copy)]
pub struct BorrowedAudioFrameBuilder<'buf> {
    data: &'buf [f32],
    sample_rate: u32,
    num_channels: u32,
    num_samples: u32,
    channel_stride_bytes: Option<usize>,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
}

impl fmt::Debug for BorrowedAudioFrameBuilder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BorrowedAudioFrameBuilder")
            .field("sample_rate", &self.sample_rate)
            .field("num_channels", &self.num_channels)
            .field("num_samples", &self.num_samples)
            .field("channel_stride_bytes", &self.channel_stride_bytes)
            .field("timecode", &self.timecode)
            .field("metadata", &self.metadata.map(CStr::to_string_lossy))
            .field("data (samples)", &self.data.len())
            .finish()
    }
}

impl<'buf> BorrowedAudioFrameBuilder<'buf> {
    /// Set the distance in bytes between consecutive channel starts.
    /// Must be a positive multiple of `size_of::<f32>()`, at least one channel's
    /// samples, and fit the SDK's signed 32-bit stride field.
    #[must_use]
    pub fn channel_stride_bytes(mut self, bytes: usize) -> Self {
        self.channel_stride_bytes = Some(bytes);
        self
    }

    /// Reset the stride override to tightly packed channels.
    #[must_use]
    pub fn clear_channel_stride_bytes(mut self) -> Self {
        self.channel_stride_bytes = None;
        self
    }

    /// Set the frame timecode; defaults to [`Timecode::SYNTHESIZE`].
    #[must_use]
    pub fn timecode(mut self, timecode: Timecode) -> Self {
        self.timecode = timecode;
        self
    }

    /// Borrow NUL-terminated UTF-8 metadata, validated when built.
    /// An explicit empty C string differs from absent metadata.
    #[must_use]
    pub fn metadata(mut self, metadata: &'buf CStr) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Remove the optional metadata override.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
        self
    }

    /// Validate the rate, positive channel/sample counts, stride, sample extent
    /// and optional UTF-8 metadata without allocating sample storage.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for an invalid layout, insufficient storage,
    /// exceeding the 64 MiB audio limit or the 4 MiB metadata cap;
    /// [`Error::InvalidUtf8`] for metadata that is not UTF-8.
    pub fn build(self) -> Result<BorrowedAudioFrame<'buf>> {
        let layout = ValidatedAudioLayout::new_uncompressed(
            AudioFormat::Fltp,
            self.sample_rate,
            self.num_channels,
            self.num_samples,
            self.channel_stride_bytes,
        )?;
        // The slice supplies alignment; the layout's extent establishes that
        // every active channel is initialized and borrowed for 'buf.
        let data = self.data.get(..layout.sample_count()).ok_or({
            Error::InvalidFrame(crate::FrameDefect::AudioBufferTooSmall {
                actual: self.data.len(),
                minimum: layout.sample_count(),
            })
        })?;
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedAudioFrame {
            layout,
            timecode: self.timecode,
            data,
            metadata,
        })
    }
}

audio_accessors! {
    impl['buf] BorrowedAudioFrame<'buf>;
    debug("BorrowedAudioFrame", "data (samples)" = sample_count);
    timecode {}
    timestamp {
        /// Application input has no SDK submission timestamp.
    }
    channel_stride_bytes {
        /// Includes any configured inter-channel padding.
    }
    channel_data {
        /// Borrows the caller's active samples without reading padding.
    }
    data(self -> Option<&[f32]>) {
        /// Contiguous active samples, or `None` when padding separates channels.
    } {
        self.contiguous_samples()
    }
    to_owned(self) {
        /// Copy initialized samples and metadata into a new tightly packed frame.
        /// The returned frame retains no borrow; padding is omitted.
    } {
        self.owned_copy()
    }
    copy_into {
        /// Copy into an existing packed frame, reusing its sample and metadata
        /// allocations when their capacity and lengths permit. A copy without
        /// metadata retains the destination's text allocation.
    }

}

/// A borrowed video frame that references external pixel data.
///
/// [`crate::Sender::send_video`] sends it synchronously; the buffer can be reused
/// as soon as the call returns. [`crate::Sender::submit_video_copy`] copies its
/// pixels and metadata into an owned frame for the asynchronous pipeline.
///
/// The x86 builds of the SDK read a little past the end of many UYVY and
/// UYVA frames. Where [`PixelFormat::send_tail_room`] is nonzero, borrowed
/// frames of these formats are copied into packed storage with a zeroed tail
/// by default, so caller padding and trailing bytes cannot affect the picture.
/// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] opts into
/// direct sending when sufficient initialized memory follows the pixels.
/// [`Self::sends_in_place`] reports whether a copy is needed.
///
/// Create one with [`Self::builder`]. Construction borrows without
/// allocation and validates the entire supported uncompressed layout once.
/// Copying it copies only the borrows.
#[derive(Clone, Copy)]
pub struct BorrowedVideoFrame<'buf> {
    layout: ValidatedVideoLayout,
    header: VideoHeader,
    /// The whole initialized buffer, including any room after the frame.
    data: &'buf [u8],
    /// Readable bytes after the last active pixel, zero unless opted in.
    room_after: usize,
    /// The metadata text, validated with its terminator.
    metadata: Option<MetadataText<'buf>>,
}

// SAFETY: Private fields pair the initialized byte slice with its validated
// layout after the builder checks its extent; the borrow retains validated
// terminated metadata unchanged.
// pixels() derives from the whole borrowed buffer, so its provenance covers
// the storage and, only after the caller opts in, the reported room after the
// last pixel. Direct SDK reads are decided by the common rule over that room.
unsafe impl crate::frames::VideoParts for BorrowedVideoFrame<'_> {
    #[inline]
    fn layout(&self) -> &ValidatedVideoLayout {
        &self.layout
    }
    #[inline]
    fn header(&self) -> &VideoHeader {
        &self.header
    }
    #[inline]
    fn pixels(&self) -> *const u8 {
        self.data.as_ptr()
    }
    #[inline]
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        FrameMetadataRef::from_text(self.metadata)
    }
    /// A frame built for sending carries no timestamp; [`Self::to_raw`]
    /// sends none.
    #[inline]
    fn sdk_timestamp(&self) -> Option<crate::SdkTimestamp> {
        None
    }

    fn initialized(&self) -> crate::frames::Initialized {
        crate::frames::Initialized::Storage {
            room_after: self.room_after,
        }
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
        /// [`Self::storage`] includes, so the active pixels of a padded buffer
        /// can be read in place.
    }
    timestamp {
        /// Application input has no SDK submission timestamp.
    }
    data(self -> Option<&[u8]>) {
        /// Contiguous active pixels, or `None` when row or plane padding separates
        /// them. Use [`Self::plane`] and [`Self::planes`] for padded active rows, or
        /// [`Self::storage`] for the validated storage including row padding.
    } { crate::frames::VideoPartsExt::active_data(self) }
    to_owned(self) {
        /// Copy this borrowed frame into a validated owned frame.
        ///
        /// Active pixels are copied into tightly packed planes; row padding is
        /// skipped and the owned stride can shrink. Metadata is copied separately;
        /// no reference to the source is retained. [`Self::copy_into`] makes the
        /// same copy into an existing frame, reusing its allocations.
        ///
        /// The copy cannot fail: building the frame validated everything it
        /// relies on, and the packed layout of a valid frame is valid.
    } {
        crate::frames::VideoPartsExt::owned_copy(self)
    }
    copy_into {
        /// Copy this borrowed frame into `destination`, reusing its allocations.
        ///
        /// `destination` ends up exactly as [`Self::to_owned`] would have
        /// returned it, whatever it held before: tightly packed active rows,
        /// format, dimensions, frame rate, aspect ratio, scan type, timecode and
        /// metadata, and no timestamp. No reference to the source is retained.
        ///
        /// Its pixel buffer is reused when its capacity holds the packed frame
        /// and the SDK tail room required by its format and scan type, including
        /// extra strides for interlaced UYVY/UYVA when needed. Otherwise it is
        /// replaced by one allocation of exactly that size. Its metadata string
        /// is reused when its capacity holds the new text and its terminator;
        /// otherwise it grows once. With sufficient pixel and metadata capacity,
        /// subsequent copies allocate nothing. The copy makes no SDK call and can
        /// run on any thread. Copies without metadata retain the destination's
        /// text allocation for later frames.
        ///
        /// # Example
        ///
        /// A copy-and-send loop that stops allocating once two frames exist.
        /// [`crate::Sender::submit_video`] keeps the frame it is given until the next
        /// send, and returns the previous one, which the SDK has finished with:
        /// that frame is the destination of the next copy.
        /// [`crate::Sender::submit_video_copy`] keeps its own reusable frames for
        /// this copy instead.
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
        ///         .line_stride_bytes(512)
        ///         .build()?;
        ///     let owned = match spare.take() {
        ///         Some(mut reused) => {
        ///             frame.copy_into(&mut reused); // no allocation
        ///             reused
        ///         }
        ///         None => frame.to_owned(), // the first two frames only
        ///     };
        ///     spare = sender.submit_video(owned)?;
        /// }
        /// sender.reclaim_video();
        /// # Ok(())
        /// # }
        /// ```
    }
    sends_in_place {
        /// Whether [`crate::Sender::send_video`] sends this frame from its buffer,
        /// without copying its pixels.
        ///
        /// UYVY and UYVA frames are copied by default wherever
        /// [`PixelFormat::send_tail_room`] is nonzero. Opting in with
        /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] allows
        /// direct sending if enough room follows the last pixel. The last row's
        /// padding and the bytes of the builder's buffer after the frame count
        /// towards the room after that opt-in. An interlaced or single-field
        /// frame of those formats whose height is not a multiple of four needs
        /// two line strides more.
        ///
        /// Where the SDK reads frames exactly, which is 64-bit ARM Linux, the
        /// room is zero and every frame sends in place.
        ///
        /// # Example
        ///
        /// ```
        /// # use grafton_ndi::{BorrowedVideoFrame, PixelFormat, VideoFrame};
        /// # fn main() -> Result<(), grafton_ndi::Error> {
        /// let uyvy = PixelFormat::Uyvy;
        /// let room = uyvy.send_tail_room();
        /// let exact = vec![0u8; 720 * 480 * 2];
        /// let frame = BorrowedVideoFrame::builder(&exact, 720, 480, uyvy).build()?;
        /// // Copied wherever the SDK needs room after the frame.
        /// assert_eq!(frame.sends_in_place(), room == 0);
        ///
        /// let longer = vec![0u8; 720 * 480 * 2 + room];
        /// let default = BorrowedVideoFrame::builder(&longer, 720, 480, uyvy).build()?;
        /// assert_eq!(default.sends_in_place(), room == 0);
        /// let in_place = BorrowedVideoFrame::builder(&longer, 720, 480, uyvy)
        ///     .allow_send_from_padding_and_tail(true)
        ///     .build()?;
        /// assert!(in_place.sends_in_place());
        /// assert_eq!(in_place.data().unwrap().len(), 720 * 480 * 2);
        ///
        /// let owned = VideoFrame::builder()
        ///     .resolution(720, 480)
        ///     .pixel_format(uyvy)
        ///     .build()?;
        /// assert!(owned.sends_in_place());
        /// # Ok(())
        /// # }
        /// ```
    }
    images("", "\
 # use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
 # let pixels = vec![128; 64 * 36 * 2];
 # let frame = BorrowedVideoFrame::builder(&pixels, 64, 36, PixelFormat::Uyvy).build()?;") {
        /// It encodes the application's own buffer in place, padded rows
        /// included, so a program can keep a still of what it sends without
        /// copying the frame.
    }
}

impl<'buf> BorrowedVideoFrame<'buf> {
    /// Configure a checked borrowed frame over `data`.
    ///
    /// `data` may be longer than the frame: the frame is its start, and
    /// [`Self::storage`] never includes the rest. For UYVY/UYVA where the SDK
    /// needs tail room, the default send copies active rows only, even when
    /// `data` includes extra room. Direct sending requires an explicit
    /// [`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail`] opt-in.
    ///
    /// Its defaults are the packed line stride, 60/1 frames per second,
    /// square pixels (picture aspect ratio `0.0`), progressive scan, [`Timecode::SYNTHESIZE`]
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
            frame_rate: crate::media::DEFAULT_VIDEO_FRAME_RATE,
            aspect_ratio_input: PictureAspectRatio::SQUARE_PIXELS,
            scan_type: ScanType::Progressive,
            timecode: Timecode::SYNTHESIZE,
            metadata: None,
            allow_padding_and_tail: false,
        }
    }

    /// Get the pixel data: exactly the frame's validated storage, including
    /// any row padding. Bytes of the source buffer beyond the frame are not
    /// part of it. [`Self::plane`] and [`Self::planes`] borrow the active
    /// rows alone.
    pub fn storage(&self) -> &[u8] {
        &self.data[..self.layout.data_len_bytes()]
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
        self.header = self.header.with_timecode(timecode);
        self
    }

    #[cfg(test)]
    pub(crate) fn to_raw(self) -> NDIlib_video_frame_v2_t {
        self.video_descriptor()
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
    aspect_ratio_input: PictureAspectRatio,
    scan_type: ScanType,
    timecode: Timecode,
    metadata: Option<&'buf CStr>,
    allow_padding_and_tail: bool,
}

impl std::fmt::Debug for BorrowedVideoFrameBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BorrowedVideoFrameBuilder")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_format", &self.pixel_format)
            .field("line_stride_bytes", &self.line_stride)
            .field("frame_rate", &self.frame_rate)
            .field("picture_aspect_ratio", &self.aspect_ratio_input)
            .field("scan_type", &self.scan_type)
            .field("timecode", &self.timecode)
            .field("metadata", &self.metadata.map(CStr::to_string_lossy))
            .field(
                "allow_send_from_padding_and_tail",
                &self.allow_padding_and_tail,
            )
            .field("data (bytes)", &self.data.len())
            .finish()
    }
}

impl<'buf> BorrowedVideoFrameBuilder<'buf> {
    /// Set the first-plane stride in bytes, following the
    /// [video stride rules](PixelFormat::buffer_len).
    /// Use [`Self::clear_line_stride_bytes`] for the packed stride.
    #[must_use]
    pub fn line_stride_bytes(mut self, stride: usize) -> Self {
        self.line_stride = Some(stride);
        self
    }

    /// Reset the first-plane stride override to the packed stride.
    #[must_use]
    pub fn clear_line_stride_bytes(mut self) -> Self {
        self.line_stride = None;
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
    pub fn picture_aspect_ratio(mut self, ratio: PictureAspectRatio) -> Self {
        self.aspect_ratio_input = ratio;
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

    /// Remove the optional metadata override.
    #[must_use]
    pub fn clear_metadata(mut self) -> Self {
        self.metadata = None;
        self
    }

    /// Allow direct sending from caller padding and trailing memory.
    ///
    /// Defaults to `false`. Where UYVY/UYVA sending needs tail room, the
    /// default path packs active rows and zeroes the tail. Setting this to
    /// `true` permits sending directly if the supplied buffer is large enough
    /// to include the required room after the frame. The SDK may encode row
    /// padding and trailing bytes into the picture: only opt in when their
    /// contents are suitable for transmission. All such bytes must be
    /// initialized and stable throughout the send. Insufficient room still
    /// causes a packed copy. Formats with zero tail room send directly either way.
    #[must_use]
    pub fn allow_send_from_padding_and_tail(mut self, allow: bool) -> Self {
        self.allow_padding_and_tail = allow;
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
        let picture_aspect_ratio = self.aspect_ratio_input;
        let layout = ValidatedVideoLayout::new_uncompressed(
            self.pixel_format,
            self.width,
            self.height,
            self.line_stride,
        )?;
        if self.data.len() < layout.data_len_bytes() {
            return Err(Error::InvalidFrame(
                crate::FrameDefect::VideoBufferTooSmall {
                    format: self.pixel_format,
                    actual: self.data.len(),
                    minimum: layout.data_len_bytes(),
                },
            ));
        }
        let metadata = self.metadata.map(MetadataText::from_cstr).transpose()?;
        Ok(BorrowedVideoFrame {
            layout,
            header: VideoHeader::new(
                self.frame_rate,
                picture_aspect_ratio,
                self.scan_type,
                self.timecode,
            ),
            data: self.data,
            room_after: if self.allow_padding_and_tail {
                layout.trailing_padding() + (self.data.len() - layout.data_len_bytes())
            } else {
                0
            },
            metadata,
        })
    }
}
