//! Validated layouts and borrowed active-element views.

use super::*;

/// Validated video frame layout information.
///
/// This struct holds pre-validated layout information for a video frame,
/// with a plane table that determines every storage and payload quantity.
/// Creating this struct performs all necessary bounds checking, so
/// consumers can derive those quantities without re-validation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ValidatedVideoLayout {
    /// The validated width in pixels.
    width: u32,
    /// The validated height in pixels.
    height: u32,
    /// The validated pixel format.
    pixel_format: PixelFormat,
    /// The authority for plane offsets, active rows and storage strides.
    /// Unused trailing entries are zero; every extent and sum is validated.
    plane_layouts: [VideoPlaneLayout; MAX_VIDEO_PLANES],
}

// The width, height and line stride of a valid layout are each at most its
// storage extent, so every one of them fits the SDK's signed 32-bit fields.
const_assert!({
    assert!(usize::BITS >= 32);
    assert!(MAX_VIDEO_BYTES <= i32::MAX as usize);
    // An image row has at most eight bytes per validated source pixel.
    assert!(MAX_VIDEO_BYTES <= usize::MAX / 8);
    // Send room is at most 64 bytes plus two strides. A validated stride is
    // at most the complete data size, even for a one-row frame.
    assert!(3 * MAX_VIDEO_BYTES + 64 < usize::MAX);
    assert!(MAX_METADATA_BYTES < i32::MAX as usize);
});

/// Plane views whose names carry the format's component ordering.
#[cfg(feature = "image-encoding")]
#[derive(Debug, Clone, Copy)]
pub(crate) enum PlaneRoles<'a> {
    Packed {
        pixels: VideoPlane<'a>,
    },
    PackedAlpha {
        pixels: VideoPlane<'a>,
        alpha: VideoPlane<'a>,
    },
    SemiPlanar420 {
        luma: VideoPlane<'a>,
        chroma: VideoPlane<'a>,
    },
    Planar420 {
        luma: VideoPlane<'a>,
        cb: VideoPlane<'a>,
        cr: VideoPlane<'a>,
    },
    SemiPlanar422 {
        luma: VideoPlane<'a>,
        chroma: VideoPlane<'a>,
        alpha: Option<VideoPlane<'a>>,
    },
    Planar444 {
        luma: VideoPlane<'a>,
        cb: VideoPlane<'a>,
        cr: VideoPlane<'a>,
        alpha: Option<VideoPlane<'a>>,
    },
}

impl ValidatedVideoLayout {
    /// Initialized active payload, excluding padding.
    pub(crate) const fn payload_len_bytes(&self) -> usize {
        let mut payload = 0;
        let mut index = 0;
        while index < self.plane_count() {
            let plane = self.plane_layouts[index];
            payload += plane.active_row_bytes * plane.row_count;
            index += 1;
        }
        payload
    }

    /// The validated `width` value.
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    /// The validated `height` value.
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// The validated `pixel_format` value.
    pub(crate) const fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Complete storage extent in bytes; SDK padding need not be initialized.
    pub(crate) const fn data_len_bytes(&self) -> usize {
        let last = self.plane_layouts[self.plane_count() - 1];
        last.offset + last.row_stride * last.row_count
    }

    /// The validated first-plane line stride in bytes.
    pub(crate) const fn line_stride_bytes(&self) -> usize {
        self.plane_layouts[0].row_stride
    }

    pub(super) const fn plane_count(&self) -> usize {
        self.pixel_format.info().plane_count()
    }

    /// Validate an uncompressed layout. Only `None` selects the packed stride;
    /// an explicit stride must be positive and hold the active row.
    pub(crate) fn new_uncompressed(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        line_stride: Option<usize>,
    ) -> Result<Self> {
        Self::new_uncompressed_checked(pixel_format, width, height, line_stride).map_err(Into::into)
    }

    pub(crate) fn new_uncompressed_checked(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        line_stride: Option<usize>,
    ) -> LayoutResult<Self> {
        validate_video_dimensions(width.into(), height.into())?;

        let minimum = min_video_line_stride(pixel_format, width);
        // A row larger than the cap cannot describe a frame, even with a
        // shorter explicit stride. Compute its full extent before narrowing
        // so the refusal and byte count are identical on every target.
        if minimum > MAX_VIDEO_BYTES as u64 {
            let stride = line_stride.map_or(minimum, |stride| (stride as u64).max(minimum));
            video_plane_layouts(pixel_format.info(), stride, minimum, height as u64)?;
        }
        let min_stride = minimum as usize;
        let line_stride = match line_stride {
            None => min_stride,
            Some(0) => return Err(crate::LayoutDefect::VideoZeroStride),
            Some(stride) if stride < min_stride => {
                return Err(crate::LayoutDefect::VideoShortStride {
                    stride,
                    minimum: min_stride,
                    format: pixel_format,
                    width,
                });
            }
            Some(stride) => stride,
        };

        let plane_layouts = video_plane_layouts(
            pixel_format.info(),
            line_stride as u64,
            minimum,
            height as u64,
        )?;

        Ok(Self {
            width,
            height,
            pixel_format,
            plane_layouts,
        })
    }

    /// Validate the layout of an uncompressed SDK frame: `width`, `height`
    /// and `line_stride` are the SDK's signed fields, and a zero stride
    /// selects the packed stride.
    pub(crate) fn from_sdk(
        pixel_format: PixelFormat,
        width: i32,
        height: i32,
        line_stride: i32,
    ) -> Result<Self> {
        Self::from_sdk_checked(pixel_format, width, height, line_stride).map_err(Into::into)
    }

    /// Validate SDK geometry without allocating even when it is rejected.
    /// Allocator callbacks use this before any pixel storage exists.
    pub(crate) fn from_sdk_checked(
        pixel_format: PixelFormat,
        width: i32,
        height: i32,
        line_stride: i32,
    ) -> LayoutResult<Self> {
        validate_video_dimensions(width.into(), height.into())?;
        let line_stride = usize::try_from(line_stride)
            .map_err(|_| crate::LayoutDefect::VideoSdkStride(line_stride))?;
        // Both dimensions were just checked positive.
        Self::new_uncompressed_checked(
            pixel_format,
            width.unsigned_abs(),
            height.unsigned_abs(),
            (line_stride != 0).then_some(line_stride),
        )
    }

    /// This layout with every plane's rows packed, without padding: the
    /// layout of an owned copy of the frame.
    ///
    /// It keeps each validated plane's active rows and closes the gaps: a
    /// plane's stride becomes its active row and its offset the payload of
    /// the planes before it, so the storage is exactly the active payload.
    /// Nothing is computed that validation did not already bound: every
    /// partial payload sum is at most the storage extent validation checked against
    /// [`MAX_VIDEO_BYTES`]. It is the layout [`video_plane_layouts`] gives
    /// the packed stride, `width` times the bytes per pixel, which a test
    /// checks for every format.
    pub(crate) fn packed(&self) -> Self {
        let mut plane_layouts = self.plane_layouts;
        let mut offset = 0;
        for plane in &mut plane_layouts[..self.plane_count()] {
            plane.offset = offset;
            plane.row_stride = plane.active_row_bytes;
            offset += plane.active_row_bytes * plane.row_count;
        }
        Self {
            plane_layouts,
            ..*self
        }
    }

    /// The SDK's `xres`.
    pub(crate) fn sdk_width(&self) -> i32 {
        self.width as i32
    }

    /// The SDK's `yres`.
    pub(crate) fn sdk_height(&self) -> i32 {
        self.height as i32
    }

    /// The SDK's `line_stride_in_bytes`.
    pub(crate) fn sdk_line_stride(&self) -> i32 {
        self.line_stride_bytes() as i32
    }

    /// The storage after the frame's last active byte: the padding of the
    /// last row of the last plane, where the storage ends.
    pub(crate) fn trailing_padding(&self) -> usize {
        let last = self.plane_layouts[self.plane_count() - 1];
        last.row_stride - last.active_row_bytes
    }

    pub(super) fn planes(&self) -> &[VideoPlaneLayout] {
        &self.plane_layouts[..self.plane_count()]
    }

    /// One plane of the frame stored at `data`, or `None` for an invalid
    /// index. Owned, borrowed and received frames share this view.
    ///
    /// # Safety
    ///
    /// `data` must point to storage this layout describes, with every active
    /// row initialized and immutable for `'a`.
    pub(super) unsafe fn view_plane<'a>(
        &'a self,
        data: *const u8,
        index: usize,
    ) -> Option<VideoPlane<'a>> {
        let layout = *self.planes().get(index)?;
        // SAFETY: forwarded to the caller.
        Some(unsafe { VideoPlane::new(data, layout) })
    }

    /// Every plane of the frame stored at `data`, in SDK memory order.
    ///
    /// # Safety
    ///
    /// As for [`Self::view_plane`].
    pub(crate) unsafe fn view_planes<'a>(&'a self, data: *const u8) -> VideoPlanes<'a> {
        VideoPlanes {
            data,
            layouts: self.planes().iter(),
        }
    }

    /// Every plane identified by its component role, including YV12's Cr/Cb order.
    ///
    /// # Safety
    ///
    /// As for [`Self::view_plane`].
    #[cfg(feature = "image-encoding")]
    pub(super) unsafe fn plane_roles<'a>(&'a self, data: *const u8) -> PlaneRoles<'a> {
        let info = self.pixel_format.info();
        // SAFETY: validation constructs the complete fixed plane table for the
        // category; each constant index below names one of its active planes.
        // The caller retains and initializes its storage for the returned borrow.
        let plane = |index| unsafe { VideoPlane::new(data, self.plane_layouts[index]) };
        match info.category() {
            FormatCategory::Packed => PlaneRoles::Packed { pixels: plane(0) },
            FormatCategory::PackedAlpha => PlaneRoles::PackedAlpha {
                pixels: plane(0),
                alpha: plane(1),
            },
            FormatCategory::SemiPlanar420 => PlaneRoles::SemiPlanar420 {
                luma: plane(0),
                chroma: plane(1),
            },
            FormatCategory::Planar420 => {
                let (cb, cr) = match info.chroma_order() {
                    ChromaOrder::CbCr => (plane(1), plane(2)),
                    ChromaOrder::CrCb => (plane(2), plane(1)),
                };
                PlaneRoles::Planar420 {
                    luma: plane(0),
                    cb,
                    cr,
                }
            }
            FormatCategory::SemiPlanar422 | FormatCategory::SemiPlanar422Alpha => {
                PlaneRoles::SemiPlanar422 {
                    luma: plane(0),
                    chroma: plane(1),
                    alpha: info.has_alpha().then(|| plane(2)),
                }
            }
            FormatCategory::Planar444 | FormatCategory::Planar444Alpha => PlaneRoles::Planar444 {
                luma: plane(0),
                cb: plane(1),
                cr: plane(2),
                alpha: info.has_alpha().then(|| plane(3)),
            },
        }
    }

    /// Every active byte of the frame stored at `data`, in SDK plane order,
    /// or `None` when padding separates rows or planes. A single row may
    /// have padding after it, which is not part of the bytes.
    ///
    /// The bytes are the one row of the span [`VideoPlaneLayout::contiguous`]
    /// finds, read as [`VideoPlane::data`] reads a plane's, so a frame and
    /// each of its planes share one contiguity rule and one slice.
    ///
    /// # Safety
    ///
    /// As for [`Self::view_plane`].
    pub(super) unsafe fn view_data<'a>(&self, data: *const u8) -> Option<&'a [u8]> {
        let span = VideoPlaneLayout::contiguous(self.planes())?;
        // SAFETY: the span is one row of active bytes, from the first plane's
        // offset to the end of the last plane's last active row, within the
        // storage this layout describes. The caller keeps every active row
        // initialized and immutable for `'a`.
        unsafe { VideoPlane::new(data, span) }.data()
    }
}

pub(crate) type LayoutResult<T> = std::result::Result<T, crate::LayoutDefect>;

impl From<crate::LayoutDefect> for Error {
    fn from(reason: crate::LayoutDefect) -> Self {
        Self::InvalidFrame(crate::FrameDefect::Layout(reason))
    }
}

/// The one dimension check, for values from the API and from the SDK.
pub(super) fn validate_video_dimensions(width: i64, height: i64) -> LayoutResult<()> {
    validate_video_width(width)?;
    validate_video_height(height)
}

pub(super) fn validate_video_height(height: i64) -> LayoutResult<()> {
    if height <= 0 {
        return Err(crate::LayoutDefect::VideoHeight(height.into()));
    }

    Ok(())
}

pub(super) fn validate_video_width(width: i64) -> LayoutResult<()> {
    if width <= 0 {
        return Err(crate::LayoutDefect::VideoWidth(width.into()));
    }

    // The SDK uses 4:2:2 internally, including for RGB input/output.
    // https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types
    if width % 2 != 0 {
        return Err(crate::LayoutDefect::VideoOddWidth(width.into()));
    }

    Ok(())
}

pub(super) fn validate_video_data_len(data_len_bytes: u64) -> LayoutResult<()> {
    debug_assert!(
        data_len_bytes > 0,
        "validated dimensions and stride are positive"
    );
    if data_len_bytes > MAX_VIDEO_BYTES as u64 {
        return Err(crate::LayoutDefect::VideoBytes(data_len_bytes.into()));
    }
    Ok(())
}

pub(super) fn min_video_line_stride(pixel_format: PixelFormat, width: u32) -> u64 {
    u64::from(width) * u64::from(pixel_format.info().bytes_per_pixel())
}

/// Calculate buffer length with checked arithmetic.
#[cfg(test)]
pub(super) fn calculate_buffer_len_checked(
    pixel_format: PixelFormat,
    y_stride: usize,
    height: usize,
) -> Result<usize> {
    calculate_buffer_len_for_info_checked(pixel_format.info(), y_stride, height)
}

#[cfg(test)]
pub(super) fn calculate_buffer_len_for_info_checked(
    info: PixelFormatInfo,
    y_stride: usize,
    height: usize,
) -> Result<usize> {
    video_plane_layouts(info, y_stride as u64, y_stride as u64, height as u64)
        .map(|planes| {
            let last = planes[info.plane_count() - 1];
            last.offset + last.row_stride * last.row_count
        })
        .map_err(Into::into)
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct VideoPlaneLayout {
    offset: usize,
    row_stride: usize,
    active_row_bytes: usize,
    row_count: usize,
}

impl VideoPlaneLayout {
    #[cfg(test)]
    pub(super) fn offset(&self) -> usize {
        self.offset
    }
    #[cfg(test)]
    pub(super) fn row_stride(&self) -> usize {
        self.row_stride
    }
    #[cfg(test)]
    pub(super) fn active_row_bytes(&self) -> usize {
        self.active_row_bytes
    }
    #[cfg(test)]
    pub(super) fn row_count(&self) -> usize {
        self.row_count
    }

    /// The active rows of `planes`, consecutive planes of one validated
    /// frame, as one row, or `None` when padding separates their rows or the
    /// planes: the one contiguity rule, for a frame and for each of its
    /// planes. Padding after the last row is not part of the span.
    pub(super) fn contiguous(planes: &[Self]) -> Option<Self> {
        let start = planes.first()?.offset;
        let mut end = start;
        for plane in planes {
            let rows_adjacent = plane.row_count == 1 || plane.row_stride == plane.active_row_bytes;
            if plane.offset != end || !rows_adjacent {
                return None;
            }
            // At most the plane's storage, which validation bounded.
            end = plane.offset + plane.row_count * plane.active_row_bytes;
        }
        let len = end - start;
        Some(Self {
            offset: start,
            row_stride: len,
            active_row_bytes: len,
            row_count: 1,
        })
    }
}

/// Derive SDK plane offsets and active row sizes together. See
/// https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types .
/// Plane offsets include preceding storage padding; payload lengths do not.
const MAX_VIDEO_PLANES: usize = 4;

fn video_plane_layouts(
    info: PixelFormatInfo,
    stride: u64,
    active: u64,
    height: u64,
) -> LayoutResult<[VideoPlaneLayout; MAX_VIDEO_PLANES]> {
    let mut dimensions = [(stride, active, height); MAX_VIDEO_PLANES];
    match info.category() {
        FormatCategory::Packed => {}
        FormatCategory::Planar420 => {
            if !height.is_multiple_of(2) || !stride.is_multiple_of(2) {
                return Err(crate::LayoutDefect::VideoPlanar420 {
                    height: height as usize,
                    stride: stride as usize,
                });
            }
            dimensions[1] = (stride / 2, active / 2, height / 2);
            dimensions[2] = dimensions[1];
        }
        FormatCategory::SemiPlanar420 => {
            if !height.is_multiple_of(2) {
                return Err(crate::LayoutDefect::VideoSemiPlanar420(height as usize));
            }
            dimensions[1] = (stride, active, height / 2);
        }
        FormatCategory::PackedAlpha => {
            if !stride.is_multiple_of(2) {
                return Err(crate::LayoutDefect::VideoUyvaStride(stride as usize));
            }
            dimensions[1] = (stride / 2, active / 2, height);
        }
        FormatCategory::SemiPlanar422 => {}
        FormatCategory::SemiPlanar422Alpha | FormatCategory::Planar444 => {}
        FormatCategory::Planar444Alpha => {}
    }
    let count = info.plane_count();
    // Saturation retains one size cause even when an explicit stride near
    // usize::MAX makes the mathematical extent exceed u64::MAX.
    let storage = dimensions[..count]
        .iter()
        .fold(0u64, |total, &(stride, _, rows)| {
            total.saturating_add(stride.saturating_mul(rows))
        });
    validate_video_data_len(storage)?;
    let mut planes = [VideoPlaneLayout::default(); MAX_VIDEO_PLANES];
    let mut offset = 0usize;
    for (plane, &(row_stride, active_row_bytes, row_count)) in
        planes.iter_mut().zip(&dimensions[..count])
    {
        // Each plane and partial sum fits the cap, which fits usize.
        let (row_stride, active_row_bytes, row_count) = (
            row_stride as usize,
            active_row_bytes as usize,
            row_count as usize,
        );
        *plane = VideoPlaneLayout {
            offset,
            row_stride,
            active_row_bytes,
            row_count,
        };
        offset += row_stride * row_count;
        debug_assert!(active_row_bytes <= row_stride);
    }
    // The complete storage extent and each partial sum fit MAX_VIDEO_BYTES.
    // Active rows fit their strides, so payload products and sums fit too.
    // Layout accessors and packed() can therefore derive either extent using
    // ordinary usize arithmetic without overflow on any supported target.
    Ok(planes)
}

/// Validated audio frame layout information.
///
/// This struct holds pre-validated layout information for a frame of audio
/// samples, including the channel stride and computed backing sample count.
/// Every layout describes at least one sample in at least one channel; audio
/// the SDK returns without samples is classified by
/// [`validate_returned_audio`] instead. Creating this struct performs all
/// necessary bounds checking, so consumers can safely use the cached values
/// without re-validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ValidatedAudioLayout {
    /// The validated audio format.
    format: AudioFormat,
    /// Quantities whose construction proves the SDK's signed range.
    sample_rate: SdkQuantity,
    no_channels: SdkQuantity,
    no_samples: SdkQuantity,
    /// An explicit padded stride in bytes, checked against the SDK range.
    /// `None` stores channels back to back.
    padded_stride: Option<SdkQuantity>,
    /// The validated backing sample count. For strided planar audio this may be
    /// larger than `no_channels * no_samples` because it includes inter-channel
    /// padding up to the last channel's final sample.
    sample_count: usize,
}

// Keep the layout no larger than the previous representation on every target.
const_assert!({
    #[allow(dead_code)]
    struct PreviousLayout {
        format: AudioFormat,
        sample_rate: u32,
        no_channels: usize,
        no_samples: usize,
        channel_stride_samples: usize,
        sample_count: usize,
    }
    assert!(size_of::<ValidatedAudioLayout>() <= size_of::<PreviousLayout>());
});

/// Initialized audio channel slices in channel order, without allocation.
///
/// Inter-channel padding is never included. The iterator borrows the frame and
/// keeps its sample storage alive; its immutable views support scoped thread
/// transfer even when the owning capture must be freed on its original thread.
#[derive(Clone)]
pub struct AudioChannels<'a> {
    data: *const f32,
    layout: ValidatedAudioLayout,
    indices: std::ops::Range<usize>,
    lifetime: PhantomData<&'a [f32]>,
}

// SAFETY: Private fields restrict construction to validated channel views.
// Its borrow keeps their aligned allocation alive for 'a. Each yielded slice
// covers active initialized samples only. No operation accesses a native handle,
// mutates samples or frees storage; frame destruction still follows owner rules.
unsafe impl Send for AudioChannels<'_> {}
// SAFETY: Sharing exposes only immutable samples and independent iterator state.
unsafe impl Sync for AudioChannels<'_> {}

impl fmt::Debug for AudioChannels<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioChannels")
            .field("remaining_channels", &self.len())
            .field("samples_per_channel", &self.layout.no_samples())
            .field(
                "channel_stride_bytes",
                &(self.layout.channel_stride_samples() * size_of::<f32>()),
            )
            .finish()
    }
}

impl<'a> Iterator for AudioChannels<'a> {
    type Item = &'a [f32];

    fn next(&mut self) -> Option<Self::Item> {
        let channel = self.indices.next()?;
        // SAFETY: Construction retains validated immutable samples for 'a.
        unsafe { self.layout.channel(self.data, channel) }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}

impl DoubleEndedIterator for AudioChannels<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let channel = self.indices.next_back()?;
        // SAFETY: As for Iterator::next.
        unsafe { self.layout.channel(self.data, channel) }
    }
}

impl ExactSizeIterator for AudioChannels<'_> {}
impl std::iter::FusedIterator for AudioChannels<'_> {}

// Packed channel strides are bounded by the audio byte cap. Explicit padded
// strides retain their own SDK-range proof, including for a single channel
// whose stride does not contribute to its byte extent.
const_assert!(MAX_AUDIO_BYTES <= i32::MAX as usize);

impl ValidatedAudioLayout {
    /// The validated `format` value.
    pub(crate) const fn format(&self) -> AudioFormat {
        self.format
    }

    /// The validated `sample_rate` value.
    pub(crate) const fn sample_rate(&self) -> u32 {
        self.sample_rate.get()
    }

    /// The validated `no_channels` value.
    pub(crate) const fn no_channels(&self) -> usize {
        self.no_channels.to_usize()
    }

    /// The validated `no_samples` value.
    pub(crate) const fn no_samples(&self) -> usize {
        self.no_samples.to_usize()
    }

    /// The validated `channel_stride_samples` value.
    pub(crate) fn channel_stride_samples(&self) -> usize {
        self.channel_stride_bytes() / size_of::<f32>()
    }

    /// Initialized samples only, excluding inter-channel padding.
    pub(super) fn active_sample_count(&self) -> usize {
        self.no_channels() * self.no_samples()
    }

    /// The validated `sample_count` value.
    pub(crate) const fn sample_count(&self) -> usize {
        self.sample_count
    }

    /// Validate caller-supplied audio geometry without constructing a descriptor.
    /// Only `None` selects packed channels; an explicit stride must be positive.
    pub(crate) fn new_uncompressed(
        format: AudioFormat,
        sample_rate: u32,
        no_channels: u32,
        no_samples: u32,
        channel_stride: Option<usize>,
    ) -> Result<Self> {
        Self::new_uncompressed_checked(format, sample_rate, no_channels, no_samples, channel_stride)
            .map_err(Into::into)
    }

    /// As `new_uncompressed`, retaining the cause for reusable input values.
    pub(crate) fn new_uncompressed_checked(
        format: AudioFormat,
        sample_rate: u32,
        no_channels: u32,
        no_samples: u32,
        channel_stride: Option<usize>,
    ) -> LayoutResult<Self> {
        Self::checked(
            format,
            sample_rate.into(),
            no_channels.into(),
            no_samples.into(),
            channel_stride,
        )
    }

    /// Validate signed SDK geometry. This does not examine a sample pointer;
    /// capture validates that separately, while allocators have no storage yet.
    pub(super) fn from_sdk(
        format: AudioFormat,
        sample_rate: i32,
        no_channels: i32,
        no_samples: i32,
        channel_stride: Option<i32>,
    ) -> Result<Self> {
        Self::from_sdk_checked(format, sample_rate, no_channels, no_samples, channel_stride)
            .map_err(Into::into)
    }

    /// As `from_sdk`, retaining an allocation-free failure for SDK callbacks.
    pub(crate) fn from_sdk_checked(
        format: AudioFormat,
        sample_rate: i32,
        no_channels: i32,
        no_samples: i32,
        channel_stride: Option<i32>,
    ) -> LayoutResult<Self> {
        let stride = channel_stride
            .map(|stride| {
                usize::try_from(stride).map_err(|_| crate::LayoutDefect::AudioStride(stride.into()))
            })
            .transpose()?;
        Self::checked(
            format,
            sample_rate.into(),
            no_channels.into(),
            no_samples.into(),
            stride,
        )
    }

    /// Replace the stride through the same rule used for delivered audio.
    #[cfg(feature = "advanced")]
    pub(crate) fn with_channel_stride_bytes(self, stride: usize) -> LayoutResult<Self> {
        Self::from_quantities(
            self.format,
            self.sample_rate,
            self.no_channels,
            self.no_samples,
            Some(stride),
        )
    }

    pub(super) fn checked(
        format: AudioFormat,
        sample_rate: i64,
        no_channels: i64,
        no_samples: i64,
        channel_stride: Option<usize>,
    ) -> LayoutResult<Self> {
        let sample_rate = validate_audio_sample_rate(sample_rate)?;
        let (no_channels, no_samples) = validate_positive_audio_counts(no_channels, no_samples)?;
        Self::from_quantities(format, sample_rate, no_channels, no_samples, channel_stride)
    }

    /// Check only the extent and stride of quantities already in the SDK range.
    pub(crate) fn from_quantities(
        format: AudioFormat,
        sample_rate: SdkQuantity,
        no_channels: SdkQuantity,
        no_samples: SdkQuantity,
        channel_stride: Option<usize>,
    ) -> LayoutResult<Self> {
        let minimum = u64::from(no_samples.get()) * size_of::<f32>() as u64;
        let stride = channel_stride.map_or(minimum, |stride| stride as u64);
        let sample_count =
            checked_audio_extent(no_channels, no_samples, stride / size_of::<f32>() as u64)?;
        // The extent bounds the packed minimum even for a single channel.
        let minimum = minimum as usize;
        let stride = stride as usize;
        let checked_stride = i64::try_from(stride)
            .ok()
            .and_then(SdkQuantity::new)
            .ok_or_else(|| {
                if stride == 0 {
                    crate::LayoutDefect::AudioStride(0.into())
                } else {
                    crate::LayoutDefect::AudioStrideRange
                }
            })?;
        if !stride.is_multiple_of(size_of::<f32>()) {
            return Err(crate::LayoutDefect::AudioStrideAlignment(stride));
        }
        if stride < minimum {
            return Err(crate::LayoutDefect::AudioShortStride { stride, minimum });
        }
        let padded_stride = (stride != minimum).then_some(checked_stride);
        Ok(Self {
            format,
            sample_rate,
            no_channels,
            no_samples,
            padded_stride,
            sample_count,
        })
    }

    /// The same frame with channels stored back to back, without padding.
    pub(crate) fn packed(self) -> Self {
        Self {
            padded_stride: None,
            sample_count: self.no_channels() * self.no_samples(),
            ..self
        }
    }

    pub(crate) fn num_channels(self) -> u32 {
        self.no_channels.get()
    }

    pub(crate) fn num_samples(self) -> u32 {
        self.no_samples.get()
    }

    pub(crate) fn channel_stride_bytes(self) -> usize {
        self.padded_stride.map_or_else(
            || self.no_samples() * size_of::<f32>(),
            SdkQuantity::to_usize,
        )
    }

    /// The SDK's `sample_rate`.
    pub(crate) fn sdk_sample_rate(self) -> i32 {
        self.sample_rate.to_sdk()
    }

    /// The SDK's `no_channels`.
    pub(crate) fn sdk_num_channels(self) -> i32 {
        self.no_channels.to_sdk()
    }

    /// The SDK's `no_samples`.
    pub(crate) fn sdk_num_samples(self) -> i32 {
        self.no_samples.to_sdk()
    }

    /// The SDK's `channel_stride_in_bytes`.
    pub(crate) fn sdk_channel_stride(self) -> i32 {
        self.padded_stride.map_or_else(
            // Packed stride fits the audio byte cap, which is at most i32::MAX.
            || (self.no_samples() * size_of::<f32>()) as i32,
            SdkQuantity::to_sdk,
        )
    }

    pub(crate) fn channel_range(self, channel: usize) -> Option<std::ops::Range<usize>> {
        if channel >= self.no_channels() {
            return None;
        }

        // checked_audio_extent proved the last channel's offset plus its
        // samples fits usize and sample_count; every earlier channel fits too.
        // packed() preserves that proof when removing inter-channel padding.
        let start = channel * self.channel_stride_samples();
        let end = start + self.no_samples();
        debug_assert!(end <= self.sample_count);
        Some(start..end)
    }

    /// One channel's samples of the frame stored at `data`, or `None` for an
    /// invalid channel. Owned and received frames share this view.
    ///
    /// # Safety
    ///
    /// `data` must point to `f32`-aligned storage this layout describes, with
    /// every channel's samples initialized and immutable for `'a`.
    pub(super) unsafe fn channel<'a>(self, data: *const f32, channel: usize) -> Option<&'a [f32]> {
        let range = self.channel_range(channel)?;
        // SAFETY: checked_audio_extent and packed() prove every channel's
        // range fits the initialized storage the caller keeps borrowed.
        Some(unsafe { slice::from_raw_parts(data.add(range.start), self.no_samples()) })
    }

    /// Every channel's samples of the frame stored at `data`, in order,
    /// without reading inter-channel padding.
    ///
    /// # Safety
    ///
    /// As for [`Self::channel`].
    pub(crate) unsafe fn channels<'a>(self, data: *const f32) -> AudioChannels<'a> {
        AudioChannels {
            data,
            layout: self,
            indices: 0..self.no_channels(),
            lifetime: PhantomData,
        }
    }

    /// Every sample of the frame stored at `data`, channels back to back, or
    /// `None` when padding separates channels: the one contiguity rule for
    /// audio, beside the channel views. An owned frame's channels are always
    /// back to back.
    ///
    /// # Safety
    ///
    /// As for [`Self::channel`].
    pub(super) unsafe fn view_samples<'a>(self, data: *const f32) -> Option<&'a [f32]> {
        if self.no_channels() > 1 && self.padded_stride.is_some() {
            return None;
        }
        // SAFETY: with one channel, or channels back to back, the
        // `sample_count` samples from `data` are every channel's samples and
        // nothing else, which the caller keeps aligned, initialized and
        // immutable for `'a`.
        Some(unsafe { slice::from_raw_parts(data, self.sample_count) })
    }
}

pub(super) fn validate_outbound_audio_layout(
    sample_rate: u32,
    no_channels: u32,
    no_samples: u32,
    format: AudioFormat,
) -> Result<ValidatedAudioLayout> {
    ValidatedAudioLayout::new_uncompressed(format, sample_rate, no_channels, no_samples, None)
}

/// The one sample-rate check for every audio path, for values from the API
/// and from the SDK: `1..=i32::MAX` Hz.
pub(crate) fn validate_audio_sample_rate(sample_rate: i64) -> LayoutResult<SdkQuantity> {
    SdkQuantity::new(sample_rate).ok_or(crate::LayoutDefect::AudioRate(sample_rate.into()))
}

/// The one audio count rule for channels and samples: `1..=i32::MAX`.
pub(crate) fn validate_audio_count(
    value: i64,
    kind: crate::AudioCountKind,
) -> LayoutResult<SdkQuantity> {
    SdkQuantity::new(value).ok_or(crate::LayoutDefect::AudioCount {
        value: value.into(),
        kind,
    })
}

/// A zero SDK field means the source quantity is not known yet. Every known
/// quantity follows the same rule as a frame that contains samples.
pub(super) fn optional_audio_quantity(
    value: i64,
    check: impl FnOnce(i64) -> LayoutResult<SdkQuantity>,
) -> LayoutResult<Option<SdkQuantity>> {
    if value == 0 {
        Ok(None)
    } else {
        check(value).map(Some)
    }
}

/// Retain the signed-range proof for channel and sample counts.
pub(crate) fn validate_positive_audio_counts(
    no_channels: i64,
    no_samples: i64,
) -> LayoutResult<(SdkQuantity, SdkQuantity)> {
    Ok((
        validate_audio_count(no_channels, crate::AudioCountKind::Channels)?,
        validate_audio_count(no_samples, crate::AudioCountKind::Samples)?,
    ))
}

/// The one audio extent calculation. Returns the backing sample count from the
/// first sample of the first channel to the last sample of the last channel
/// when channels start `stride_samples` apart, enforcing `MAX_AUDIO_BYTES`.
/// Packed audio is the case `stride_samples == no_samples`.
pub(crate) fn checked_audio_extent(
    no_channels: SdkQuantity,
    no_samples: SdkQuantity,
    stride_samples: u64,
) -> LayoutResult<usize> {
    // The retained nonzero count proof makes this subtraction infallible.
    let sample_count = u64::from(no_channels.get() - 1)
        .saturating_mul(stride_samples)
        .saturating_add(u64::from(no_samples.get()));
    let byte_size = sample_count.saturating_mul(size_of::<f32>() as u64);
    if byte_size > MAX_AUDIO_BYTES as u64 {
        return Err(crate::LayoutDefect::AudioBytes(byte_size.into()));
    }
    Ok(sample_count as usize)
}

/// An allocation-free view of one video plane's initialized active bytes.
///
/// Plane order follows the pixel format: packed pixels; UYVY/A for UYVA;
/// Y/CbCr[/A] for P216/PA16; Y/V/U for YV12; Y/U/V for I420; Y/UV for NV12;
/// Y/U/V[/A] for the planar 4:4:4 formats. The view borrows its frame, owned
/// or captured, which keeps the allocation alive.
/// Immutable pixel views are `Send + Sync`, including views borrowed from
/// FrameSync. Scoped workers must finish before the frame is freed. This does
/// not transfer the SDK-owning frame reference or change its freeing thread.
///
/// ```no_run
/// # fn read(frame: &grafton_ndi::FrameSyncVideoRef<'_>) {
/// let plane = frame.plane(0).unwrap();
/// let rows = plane.rows();
/// std::thread::scope(|scope| {
///     let worker = scope.spawn(move || {
///         rows.flatten().map(|&byte| u64::from(byte)).sum::<u64>()
///     });
///     assert_eq!(worker.join().unwrap(),
///         plane.rows().flatten().map(|&byte| u64::from(byte)).sum::<u64>());
/// });
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct VideoPlane<'a> {
    data: *const u8,
    layout: VideoPlaneLayout,
    lifetime: PhantomData<&'a [u8]>,
}

// SAFETY: Every private construction site retains one allocation for 'a and
// validates all offsets, strides, and active lengths before constructing views.
// Active bytes are initialized and immutable for 'a; padding is never sliced.
// Reads require only u8 alignment (1) and expose the same &'a [u8] already
// returned by row/data. Copying or sharing a view grants no mutation or SDK
// access. Views have no destructor/free operation. Their borrow retains the
// captured guard until all workers finish, so FrameSync still frees its frame
// on the capture thread. Owned-slice construction likewise retains its borrow.
// SDK ownership/free boundary: Processing.NDI.Recv.h (capture_v3/free_video_v2)
// and Processing.NDI.FrameSync.h (capture_video/free_video).
unsafe impl Send for VideoPlane<'_> {}
// SAFETY: Same immutable access and lifetime proof as Send above.
unsafe impl Sync for VideoPlane<'_> {}

// This formatter reads only validated extents, never the plane's allocation.
struct PlaneExtent<'a>(&'a VideoPlaneLayout);

impl fmt::Debug for PlaneExtent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoPlane")
            .field("row_stride_bytes", &self.0.row_stride)
            .field("active_row_bytes", &self.0.active_row_bytes)
            .field("row_count", &self.0.row_count)
            .finish()
    }
}

impl fmt::Debug for VideoPlane<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        PlaneExtent(&self.layout).fmt(f)
    }
}

impl<'a> VideoPlane<'a> {
    /// Caller must retain an allocation covering the validated layout for 'a,
    /// with every active row readable, initialized, and immutable for that lifetime.
    unsafe fn new(data: *const u8, layout: VideoPlaneLayout) -> Self {
        Self {
            data,
            layout,
            lifetime: PhantomData,
        }
    }

    /// Distance between row starts in bytes, including padding.
    pub fn row_stride_bytes(&self) -> usize {
        self.layout.row_stride
    }

    /// Number of initialized bytes in each row.
    pub fn active_row_bytes(&self) -> usize {
        self.layout.active_row_bytes
    }

    /// Number of rows in this plane.
    pub fn row_count(&self) -> usize {
        self.layout.row_count
    }

    /// Get an active row, excluding padding, or `None` for an invalid index.
    pub fn row(&self, index: usize) -> Option<&'a [u8]> {
        if index >= self.layout.row_count {
            return None;
        }
        // SAFETY: Construction ties the allocation to 'a. Checked plane/row
        // arithmetic bounds this range within storage. The SDK initializes the
        // active bytes; padding is never included. The guard prevents freeing.
        Some(unsafe {
            slice::from_raw_parts(
                self.data
                    .add(self.layout.offset + index * self.layout.row_stride),
                self.layout.active_row_bytes,
            )
        })
    }

    /// Iterate active rows without allocating or copying pixels.
    pub fn rows(&self) -> VideoRows<'a> {
        VideoRows {
            plane: *self,
            indices: 0..self.row_count(),
        }
    }

    /// Borrow all active bytes when contiguous, excluding trailing padding.
    /// Returns `None` when padding separates rows.
    pub fn data(&self) -> Option<&'a [u8]> {
        let span = VideoPlaneLayout::contiguous(slice::from_ref(&self.layout))?;
        // SAFETY: Construction ties the allocation to 'a. The span is this
        // plane's rows, adjacent and active, so every byte is initialized and
        // within its checked storage; the final row's padding is excluded.
        Some(unsafe { slice::from_raw_parts(self.data.add(span.offset), span.active_row_bytes) })
    }
}

/// Active video rows, borrowing the frame that owns the allocation.
/// `Send + Sync` through its immutable plane; iteration never calls the SDK.
#[derive(Clone)]
pub struct VideoRows<'a> {
    plane: VideoPlane<'a>,
    indices: std::ops::Range<usize>,
}

impl fmt::Debug for VideoRows<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoRows")
            .field("remaining_rows", &self.len())
            .field("row_stride_bytes", &self.plane.row_stride_bytes())
            .field("active_row_bytes", &self.plane.active_row_bytes())
            .finish()
    }
}

impl<'a> Iterator for VideoRows<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        self.plane.row(self.indices.next()?)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}
impl DoubleEndedIterator for VideoRows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.plane.row(self.indices.next_back()?)
    }
}
impl ExactSizeIterator for VideoRows<'_> {}
impl std::iter::FusedIterator for VideoRows<'_> {}

/// Video planes in SDK memory order, with no allocation or pixel copies.
/// `Send + Sync` for scoped reads; the owning frame's thread rules still apply.
#[derive(Clone)]
pub struct VideoPlanes<'a> {
    data: *const u8,
    layouts: slice::Iter<'a, VideoPlaneLayout>,
}
// SAFETY: Private fields restrict construction to the validated layout
// view constructors for a live frame. The shared layout slice and data borrow last for 'a; each yielded plane
// satisfies VideoPlane's immutable, initialized, bounded u8 access proof above.
// Iterator state is exclusively mutated, clones are independent, and neither
// iteration nor dropping it accesses an SDK handle or frees the allocation.
unsafe impl Send for VideoPlanes<'_> {}
// SAFETY: Sharing the iterator exposes only immutable layouts and bytes.
unsafe impl Sync for VideoPlanes<'_> {}
impl fmt::Debug for VideoPlanes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut summary = f.debug_struct("VideoPlanes");
        summary.field("remaining_planes", &self.len());
        for layout in self.layouts.as_slice() {
            summary.field("plane", &PlaneExtent(layout));
        }
        summary.finish()
    }
}

impl<'a> Iterator for VideoPlanes<'a> {
    type Item = VideoPlane<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let layout = *self.layouts.next()?;
        // SAFETY: Private iterator fields retain a live frame with validated rows.
        Some(unsafe { VideoPlane::new(self.data, layout) })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.layouts.size_hint()
    }
}
impl ExactSizeIterator for VideoPlanes<'_> {}
impl std::iter::FusedIterator for VideoPlanes<'_> {}
