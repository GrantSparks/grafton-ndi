//! Checked native conversion between complete V210 groups and NDI P216.
//!
//! V210 is a utility buffer layout, not an NDI sendable pixel format. Each six
//! pixels occupy four 32-bit words containing three 10-bit samples and two unused
//! bits. Widths must be multiples of six; partial groups are deliberately rejected.
//! These are sample-packing conversions, without color-space or HDR transforms.

use crate::{
    frames::{ValidatedVideoLayout, MAX_VIDEO_BYTES},
    sys, Error, Ndi, PixelFormat, Result, VideoFrame,
};

/// Validated V210 storage geometry, including optional initialized row padding.
///
/// Width must be positive and divisible by six. Height must be positive. Rows
/// contain at least `width / 6 * 16` bytes; their stride must be divisible by four.
/// Both the V210 storage and equivalent P216 frame must fit the 256 MiB video cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct V210Layout {
    width: u32,
    height: u32,
    stride: usize,
    words: usize,
}

impl V210Layout {
    /// Create a tightly packed layout of complete six-pixel groups.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] for a width that is zero or not a multiple of
    /// six, a zero height, or storage beyond the video limit.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let stride = Self::active_words_checked(width)?
            .checked_mul(4)
            .ok_or_else(|| Error::InvalidFrame("V210 active row size overflow".into()))?;
        Self::with_line_stride(width, height, stride)
    }

    /// Create a layout with a byte stride that may include row padding.
    ///
    /// # Errors
    ///
    /// As for [`Self::new`], and for a stride that is not a multiple of four
    /// or does not cover every complete pixel group.
    pub fn with_line_stride(width: u32, height: u32, stride_bytes: usize) -> Result<Self> {
        let active_words = Self::active_words_checked(width)?;
        // Reuse the established even-width/height/extent validation for the
        // corresponding two full-height, 16-bit P216 planes.
        ValidatedVideoLayout::new_uncompressed(PixelFormat::P216, width, height, None)?;
        if stride_bytes == 0 || !stride_bytes.is_multiple_of(4) || stride_bytes / 4 < active_words {
            return Err(Error::InvalidFrame(
                "V210 stride must be a multiple of four and cover every complete pixel group"
                    .into(),
            ));
        }
        let bytes = stride_bytes
            .checked_mul(height as usize)
            .filter(|bytes| *bytes <= MAX_VIDEO_BYTES)
            .ok_or_else(|| Error::InvalidFrame("V210 storage exceeds the video limit".into()))?;
        Ok(Self {
            width,
            height,
            stride: stride_bytes,
            words: bytes / 4,
        })
    }

    fn active_words_checked(width: u32) -> Result<usize> {
        if width == 0 || !width.is_multiple_of(6) {
            return Err(Error::InvalidFrame(
                "V210 conversion requires a positive width divisible by six".into(),
            ));
        }
        (width as usize / 6)
            .checked_mul(4)
            .ok_or_else(|| Error::InvalidFrame("V210 active row size overflow".into()))
    }

    /// Pixel width, always a multiple of six.
    pub const fn width(self) -> u32 {
        self.width
    }
    /// Positive row count.
    pub const fn height(self) -> u32 {
        self.height
    }
    /// Byte stride between V210 rows.
    pub const fn line_stride_bytes(self) -> usize {
        self.stride
    }
    /// Complete required storage, including row padding, in 32-bit words.
    pub const fn required_words(self) -> usize {
        self.words
    }
    /// Active 32-bit words in each row, excluding padding.
    pub const fn active_words_per_row(self) -> usize {
        self.width as usize / 6 * 4
    }

    /// The SDK's `line_stride_in_bytes`.
    fn sdk_line_stride(self) -> i32 {
        // Lossless: the stride is at most the storage, which is within the
        // video limit, below i32::MAX.
        self.stride as i32
    }

    fn check_storage(self, words: usize) -> Result<()> {
        if words < self.words {
            return Err(Error::InvalidFrame(format!(
                "V210 buffer has {words} words, requires {}",
                self.words
            )));
        }
        Ok(())
    }

    fn check_p216(self, frame: &VideoFrame) -> Result<()> {
        if frame.pixel_format() != PixelFormat::P216
            || frame.width() != self.width
            || frame.height() != self.height
        {
            return Err(Error::InvalidFrame(
                "P216 frame format and dimensions must match the V210 layout".into(),
            ));
        }
        // VideoFrame owns bytes. A custom Rust global allocator need only align
        // that allocation for u8, whereas these native helpers access u16 samples.
        if !(frame.data().as_ptr() as usize).is_multiple_of(std::mem::align_of::<u16>()) {
            return Err(Error::InvalidFrame(
                "P216 sample storage must be aligned for 16-bit access".into(),
            ));
        }
        Ok(())
    }
}

/// Native V210/P216 conversion helpers retained with their selected runtime.
///
/// Both exports are resolved before construction succeeds. Operations use
/// caller-owned initialized storage and do not allocate pixel buffers.
pub struct VideoConverter {
    _ndi: Ndi,
    conversion: &'static sys::VideoConversion,
}

impl VideoConverter {
    /// Resolve both native utility functions from one initialized runtime.
    pub fn new(ndi: &Ndi) -> Result<Self> {
        Ok(Self {
            conversion: ndi.table()?,
            _ndi: ndi.clone(),
        })
    }

    /// Unpack V210 into an existing P216 frame with matching dimensions.
    ///
    /// `source` contains native 32-bit V210 words and complete row storage. Only
    /// samples are replaced: destination timecode, timestamps, metadata and frame
    /// headers remain unchanged. P216 samples contain the ten bits shifted left
    /// by six. Extra source words and row padding are ignored.
    pub fn v210_to_p216(
        &self,
        layout: V210Layout,
        source: &[u32],
        destination: &mut VideoFrame,
    ) -> Result<()> {
        layout.check_storage(source.len())?;
        layout.check_p216(destination)?;
        let mut output = destination.to_raw();
        output.p_data = destination.data_mut().as_mut_ptr();
        let mut input = output;
        // The native utility ignores the source FourCC; V210 deliberately is
        // not a PixelFormat variant and cannot enter the ordinary sending path.
        input.FourCC = u32::from_le_bytes(*b"V210");
        input.p_data = source.as_ptr().cast_mut().cast();
        input.__bindgen_anon_1.line_stride_in_bytes = layout.sdk_line_stride();
        // SAFETY: Checked complete groups, native integer alignment, matching
        // extents and disjoint Rust borrows cover all accessed pixels.
        unsafe {
            self.conversion
                .NDIlib_util_V210_to_P216(&input, &mut output)
        };
        Ok(())
    }

    /// Pack P216 into reusable V210 storage, discarding each sample's low six bits.
    ///
    /// Active output words are initialized to zero before the native call because
    /// the SDK preserves unused bits through read/modify/write operations. Their
    /// two unused high bits therefore remain zero. Row padding and extra trailing
    /// words are unchanged. V210 utility buffers carry no frame metadata.
    pub fn p216_to_v210(
        &self,
        source: &VideoFrame,
        layout: V210Layout,
        destination: &mut [u32],
    ) -> Result<()> {
        layout.check_p216(source)?;
        layout.check_storage(destination.len())?;
        let stride_words = layout.stride / 4;
        let active = layout.active_words_per_row();
        for row in destination[..layout.words].chunks_exact_mut(stride_words) {
            row[..active].fill(0);
        }
        let input = source.to_raw();
        let mut output = input;
        output.p_data = destination.as_mut_ptr().cast();
        output.__bindgen_anon_1.line_stride_in_bytes = layout.sdk_line_stride();
        // SAFETY: Typed output is initialized and aligned for native u32 access;
        // source P216 dimensions/alignment and both extents were checked above.
        unsafe {
            self.conversion
                .NDIlib_util_P216_to_V210(&input, &mut output)
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timecode;

    #[test]
    fn complete_group_geometry_and_checked_caps() {
        for width in [0, 1, 2, 4, 8, 10, i32::MAX.unsigned_abs(), u32::MAX] {
            assert!(V210Layout::new(width, 1).is_err());
        }
        for height in [0, i32::MAX.unsigned_abs(), u32::MAX] {
            assert!(V210Layout::new(6, height).is_err());
        }
        for stride in [0, 12, 17, 18, i32::MAX.unsigned_abs() as usize, usize::MAX] {
            assert!(V210Layout::with_line_stride(6, 1, stride).is_err());
        }
        for width in [6u32, 12, 18, 48, 1920, 3840, 12_288] {
            for height in [1u32, 3, 1080, 2160, 65_536] {
                for padding in [0, 4, 128] {
                    let stride = width as usize / 6 * 16 + padding;
                    let actual = V210Layout::with_line_stride(width, height, stride);
                    let packed = stride as i128 * i128::from(height);
                    let planar = i128::from(width) * i128::from(height) * 4;
                    assert_eq!(
                        actual.is_ok(),
                        packed <= MAX_VIDEO_BYTES as i128 && planar <= MAX_VIDEO_BYTES as i128
                    );
                    if let Ok(layout) = actual {
                        assert_eq!(layout.width(), width);
                        assert_eq!(layout.height(), height);
                        assert_eq!(layout.required_words(), packed as usize / 4);
                        assert_eq!(layout.line_stride_bytes(), stride);
                        assert_eq!(layout.active_words_per_row(), width as usize / 6 * 4);
                    }
                }
            }
        }
    }

    fn triplet(a: u32, b: u32, c: u32) -> u32 {
        a | b << 10 | c << 20
    }

    #[test]
    fn native_complete_groups_roundtrip_with_independent_sample_oracle() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = VideoConverter::new(&ndi).unwrap();
        let y = |x: usize, row: usize| ((x * 37 + row * 61 + 17) & 1023) as u32;
        let u = |pair: usize, row: usize| ((pair * 53 + row * 19 + 101) & 1023) as u32;
        let v = |pair: usize, row: usize| ((pair * 29 + row * 43 + 509) & 1023) as u32;
        for width in [6, 12, 18, 48, 1920] {
            for height in [1, 3] {
                for padding_words in [0, 1, 4] {
                    let layout = V210Layout::with_line_stride(
                        width,
                        height,
                        width as usize / 6 * 16 + padding_words * 4,
                    )
                    .unwrap();
                    let stride = layout.line_stride_bytes() / 4;
                    let mut packed = vec![0xa5a5_a5a5u32; layout.required_words() + 2];
                    let source = &mut packed[1..layout.required_words() + 1];
                    for row in 0..height as usize {
                        for x in (0..width as usize).step_by(6) {
                            let words = &mut source[row * stride + x / 6 * 4..][..4];
                            words.copy_from_slice(&[
                                triplet(u(x / 2, row), y(x, row), v(x / 2, row)),
                                triplet(y(x + 1, row), u(x / 2 + 1, row), y(x + 2, row)),
                                triplet(v(x / 2 + 1, row), y(x + 3, row), u(x / 2 + 2, row)),
                                triplet(y(x + 4, row), v(x / 2 + 2, row), y(x + 5, row)),
                            ]);
                            for word in words {
                                *word |= 0xc000_0000;
                            }
                        }
                    }
                    let mut frame = VideoFrame::builder()
                        .resolution(width, height)
                        .pixel_format(PixelFormat::P216)
                        .timecode(Timecode::from_ticks(123))
                        .metadata("<preserve/>")
                        .build()
                        .unwrap();
                    let address = frame.data().as_ptr();
                    converter.v210_to_p216(layout, source, &mut frame).unwrap();
                    assert_eq!(frame.data().as_ptr(), address);
                    assert_eq!(frame.timecode(), Timecode::from_ticks(123));
                    assert_eq!(frame.timestamp(), None);
                    assert_eq!(frame.metadata(), Some("<preserve/>"));
                    let plane_samples = width as usize * height as usize;
                    for row in 0..height as usize {
                        for x in 0..width as usize {
                            let sample = row * width as usize + x;
                            let read = |index: usize| {
                                u16::from_ne_bytes(
                                    frame.data()[index * 2..index * 2 + 2].try_into().unwrap(),
                                )
                            };
                            assert_eq!(read(sample), (y(x, row) << 6) as u16);
                            assert_eq!(
                                read(plane_samples + sample),
                                ((if x % 2 == 0 {
                                    u(x / 2, row)
                                } else {
                                    v(x / 2, row)
                                }) << 6) as u16
                            );
                        }
                    }
                    // Low six P216 bits are discarded; output unused V210 bits
                    // are canonical zero rather than inherited from old storage.
                    for bytes in frame.data_mut().as_chunks_mut::<2>().0 {
                        let sample = u16::from_ne_bytes(*bytes) | 63;
                        bytes.copy_from_slice(&sample.to_ne_bytes());
                    }
                    let mut back = vec![0x5a5a_5a5au32; layout.required_words() + 2];
                    converter
                        .p216_to_v210(&frame, layout, &mut back[1..layout.required_words() + 1])
                        .unwrap();
                    for row in 0..height as usize {
                        for word in 0..layout.active_words_per_row() {
                            assert_eq!(
                                back[1 + row * stride + word],
                                source[row * stride + word] & 0x3fff_ffff
                            );
                        }
                        for word in layout.active_words_per_row()..stride {
                            assert_eq!(back[1 + row * stride + word], 0x5a5a_5a5a);
                        }
                    }
                    assert_eq!(packed[0], 0xa5a5_a5a5);
                    assert_eq!(*packed.last().unwrap(), 0xa5a5_a5a5);
                    assert_eq!(back[0], 0x5a5a_5a5a);
                    assert_eq!(*back.last().unwrap(), 0x5a5a_5a5a);
                }
            }
        }
    }

    #[test]
    fn invalid_buffers_and_mismatched_frames_are_rejected_before_writes() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = VideoConverter::new(&ndi).unwrap();
        let layout = V210Layout::new(6, 1).unwrap();
        let mut output = [0x1234_5678; 4];
        let wrong = VideoFrame::builder().resolution(6, 1).build().unwrap();
        assert!(converter.p216_to_v210(&wrong, layout, &mut output).is_err());
        assert_eq!(output, [0x1234_5678; 4]);
        let mut frame = VideoFrame::builder()
            .resolution(6, 1)
            .pixel_format(PixelFormat::P216)
            .build()
            .unwrap();
        assert!(converter.v210_to_p216(layout, &[0; 3], &mut frame).is_err());
        assert!(converter
            .p216_to_v210(&frame, layout, &mut output[..3])
            .is_err());
        assert_eq!(output, [0x1234_5678; 4]);
        let other_size = V210Layout::new(12, 1).unwrap();
        assert!(converter
            .v210_to_p216(other_size, &[0; 8], &mut frame)
            .is_err());
        assert!(frame.data().iter().all(|byte| *byte == 0));
    }
}
