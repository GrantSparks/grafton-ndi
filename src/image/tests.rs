//! Tests of image encoding through the frame methods, checked against an
//! `f64` reference of the conversion formulas, round trips through synthetic
//! YUV, and decoders of every codec.

use super::{jpeg, png::row_buffer, webp, Plan, Source};
use crate::{
    AlphaMode, BorrowedVideoFrame, ColorPolicy, Downscale, Error, HdrPolicy, ImageFormat,
    ImageOptions, PixelFormat, PngCompression, SampleDepth, VideoFrame, YuvMatrix, YuvRange,
};

// ---------------------------------------------------------------------------
// Frames described by their code values
// ---------------------------------------------------------------------------

/// The sampling of a YUV format: log2 of its horizontal and vertical chroma
/// reduction, its bits a sample, and whether it has alpha.
#[derive(Debug, Clone, Copy)]
struct Sampling {
    h_shift: u32,
    v_shift: u32,
    bits: u32,
    alpha: bool,
}

const YUV_FORMATS: [PixelFormat; 11] = [
    PixelFormat::Uyvy,
    PixelFormat::Uyva,
    PixelFormat::P216,
    PixelFormat::Pa16,
    PixelFormat::Nv12,
    PixelFormat::I420,
    PixelFormat::Yv12,
    PixelFormat::Na48,
    PixelFormat::Nb48,
    PixelFormat::Na4g,
    PixelFormat::Nb4g,
];

const RGB_FORMATS: [PixelFormat; 4] = [
    PixelFormat::Rgba,
    PixelFormat::Bgra,
    PixelFormat::Rgbx,
    PixelFormat::Bgrx,
];

fn sampling(format: PixelFormat) -> Sampling {
    let (h_shift, v_shift, bits, alpha) = match format {
        PixelFormat::Uyvy => (1, 0, 8, false),
        PixelFormat::Uyva => (1, 0, 8, true),
        PixelFormat::P216 => (1, 0, 16, false),
        PixelFormat::Pa16 => (1, 0, 16, true),
        PixelFormat::Nv12 | PixelFormat::I420 | PixelFormat::Yv12 => (1, 1, 8, false),
        PixelFormat::Na48 => (0, 0, 8, false),
        PixelFormat::Nb48 => (0, 0, 8, true),
        PixelFormat::Na4g => (0, 0, 16, false),
        PixelFormat::Nb4g => (0, 0, 16, true),
        other => panic!("{other:?} is not YUV"),
    };
    Sampling {
        h_shift,
        v_shift,
        bits,
        alpha,
    }
}

/// A YUV frame as code values at the format's own depth.
#[derive(Debug, Clone)]
struct Codes {
    format: PixelFormat,
    width: usize,
    height: usize,
    luma: Vec<u16>,
    /// Cb and Cr at chroma resolution.
    chroma: Vec<(u16, u16)>,
    alpha: Vec<u16>,
}

impl Codes {
    fn new(
        format: PixelFormat,
        width: usize,
        height: usize,
        mut value: impl FnMut(Site) -> u16,
    ) -> Self {
        let s = sampling(format);
        let (cw, ch) = (width >> s.h_shift, height >> s.v_shift);
        let luma = (0..width * height).map(|i| value(Site::Luma(i))).collect();
        let chroma = (0..cw * ch)
            .map(|i| (value(Site::Cb(i)), value(Site::Cr(i))))
            .collect();
        let alpha = (0..width * height).map(|i| value(Site::Alpha(i))).collect();
        Self {
            format,
            width,
            height,
            luma,
            chroma,
            alpha,
        }
    }

    fn sampling(&self) -> Sampling {
        sampling(self.format)
    }

    /// The code values of pixel (x, y): Y, Cb, Cr and alpha, the alpha the
    /// maximum when the format has none.
    fn pixel(&self, x: usize, y: usize) -> [u16; 4] {
        let s = self.sampling();
        let (cb, cr) = self.chroma[(y >> s.v_shift) * (self.width >> s.h_shift) + (x >> s.h_shift)];
        let alpha = if s.alpha {
            self.alpha[y * self.width + x]
        } else {
            ((1u32 << s.bits) - 1) as u16
        };
        [self.luma[y * self.width + x], cb, cr, alpha]
    }

    /// The packed bytes of the frame in its format's plane layout.
    fn bytes(&self) -> Vec<u8> {
        let s = self.sampling();
        let (w, h) = (self.width, self.height);
        let cw = w >> s.h_shift;
        let mut out = Vec::new();
        let mut put = |value: u16| {
            if s.bits == 8 {
                out.push(u8::try_from(value).unwrap());
            } else {
                out.extend_from_slice(&value.to_ne_bytes());
            }
        };
        let chroma = |x: usize, y: usize| self.chroma[(y >> s.v_shift) * cw + (x >> s.h_shift)];
        match self.format {
            PixelFormat::Uyvy | PixelFormat::Uyva => {
                for y in 0..h {
                    for k in 0..w / 2 {
                        let (cb, cr) = chroma(2 * k, y);
                        put(cb);
                        put(self.luma[y * w + 2 * k]);
                        put(cr);
                        put(self.luma[y * w + 2 * k + 1]);
                    }
                }
            }
            PixelFormat::P216 | PixelFormat::Pa16 => {
                self.luma.iter().for_each(|&v| put(v));
                for y in 0..h {
                    for k in 0..w / 2 {
                        let (cb, cr) = chroma(2 * k, y);
                        put(cb);
                        put(cr);
                    }
                }
            }
            PixelFormat::Nv12 => {
                self.luma.iter().for_each(|&v| put(v));
                for &(cb, cr) in &self.chroma {
                    put(cb);
                    put(cr);
                }
            }
            PixelFormat::I420 | PixelFormat::Yv12 => {
                self.luma.iter().for_each(|&v| put(v));
                let cbs = self.chroma.iter().map(|&(cb, _)| cb);
                let crs = self.chroma.iter().map(|&(_, cr)| cr);
                if self.format == PixelFormat::I420 {
                    cbs.chain(crs).for_each(&mut put);
                } else {
                    crs.chain(cbs).for_each(&mut put);
                }
            }
            _ => {
                self.luma.iter().for_each(|&v| put(v));
                self.chroma.iter().for_each(|&(cb, _)| put(cb));
                self.chroma.iter().for_each(|&(_, cr)| put(cr));
            }
        }
        if s.alpha {
            self.alpha.iter().for_each(|&v| put(v));
        }
        out
    }

    fn frame(&self, metadata: Option<&str>) -> VideoFrame {
        let mut builder = VideoFrame::builder()
            .resolution(self.width as u32, self.height as u32)
            .pixel_format(self.format)
            .data(self.bytes());
        if let Some(metadata) = metadata {
            builder = builder.metadata(metadata);
        }
        builder.build().unwrap()
    }
}

/// Which code value a generator is asked for.
#[derive(Debug, Clone, Copy)]
enum Site {
    Luma(usize),
    Cb(usize),
    Cr(usize),
    Alpha(usize),
}

/// The spec's boundary codes at 8 bits.
const BOUNDARY: [u16; 16] = [
    0, 1, 15, 16, 17, 127, 128, 129, 234, 235, 236, 239, 240, 241, 254, 255,
];

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Boundary codes in every combination of Cb and Cr and cycling through luma,
/// with random low bits at 16 bits.
fn boundary_codes(format: PixelFormat, width: usize, height: usize) -> Codes {
    let bits = sampling(format).bits;
    let mut random = XorShift(0x5EED ^ format as u64);
    Codes::new(format, width, height, |site| {
        let boundary = match site {
            Site::Luma(i) | Site::Alpha(i) => BOUNDARY[(i * 7) % 16],
            Site::Cb(i) => BOUNDARY[i % 16],
            Site::Cr(i) => BOUNDARY[(i / 16) % 16],
        };
        if bits == 8 {
            boundary
        } else {
            (boundary << 8) | (random.next() as u16 & 0xFF)
        }
    })
}

/// Every code random.
fn random_codes(format: PixelFormat, width: usize, height: usize, seed: u64) -> Codes {
    let mask = if sampling(format).bits == 8 {
        0xFF
    } else {
        0xFFFF
    };
    let mut random = XorShift(seed);
    Codes::new(format, width, height, |_| random.next() as u16 & mask)
}

// ---------------------------------------------------------------------------
// The reference
// ---------------------------------------------------------------------------

/// The conversion formulas in `f64`, from code values at `bits` bits to RGB
/// at `max`.
fn reference_rgb(
    matrix: YuvMatrix,
    range: YuvRange,
    bits: u32,
    max: f64,
    [y, cb, cr]: [u16; 3],
) -> [f64; 3] {
    let code = |value: u16| f64::from(value) * f64::from(1u32 << (16 - bits));
    let (y, cb, cr) = (code(y), code(cb), code(cr));
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        _ => (0.2627, 0.0593),
    };
    let (luma, pb, pr) = match range {
        YuvRange::Limited => (
            (y - 4096.0) / 56064.0,
            (cb - 32768.0) / 57344.0,
            (cr - 32768.0) / 57344.0,
        ),
        _ => {
            let span = f64::from(((1u32 << bits) - 1) << (16 - bits));
            (y / span, (cb - 32768.0) / span, (cr - 32768.0) / span)
        }
    };
    let r = luma + 2.0 * (1.0 - kr) * pr;
    let b = luma + 2.0 * (1.0 - kb) * pb;
    let g = (luma - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|value| (value.clamp(0.0, 1.0) * max).round())
}

/// The alpha of a source code at `bits` bits in an image at `max`.
fn reference_alpha(alpha: u16, bits: u32, max: f64) -> f64 {
    (f64::from(alpha) / f64::from((1u32 << bits) - 1) * max).round()
}

// ---------------------------------------------------------------------------
// Decoders
// ---------------------------------------------------------------------------

/// A decoded PNG: width, height, channels, bits a channel and samples.
#[derive(Debug, PartialEq, Eq)]
struct Decoded {
    width: u32,
    height: u32,
    channels: usize,
    bits: u32,
    samples: Vec<u16>,
}

fn decode_png(bytes: &[u8]) -> Decoded {
    let mut reader = ::png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    buffer.truncate(info.buffer_size());
    let channels = match info.color_type {
        ::png::ColorType::Rgba => 4,
        ::png::ColorType::Rgb => 3,
        other => panic!("unexpected PNG color type {other:?}"),
    };
    let (bits, samples) = match info.bit_depth {
        ::png::BitDepth::Eight => (8, buffer.iter().map(|&v| u16::from(v)).collect()),
        ::png::BitDepth::Sixteen => (
            16,
            buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| u16::from_be_bytes(b))
                .collect(),
        ),
        other => panic!("unexpected PNG depth {other:?}"),
    };
    Decoded {
        width: info.width,
        height: info.height,
        channels,
        bits,
        samples,
    }
}

fn decode_webp(bytes: &[u8]) -> Decoded {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).unwrap();
    let (width, height) = decoder.dimensions();
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let mut buffer = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.read_image(&mut buffer).unwrap();
    Decoded {
        width,
        height,
        channels,
        bits: 8,
        samples: buffer.into_iter().map(u16::from).collect(),
    }
}

/// The chunk types of a PNG in order.
fn png_chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut chunks = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
        chunks.push((kind, bytes[at + 8..at + 8 + len].to_vec()));
        at += 12 + len;
    }
    chunks
}

fn options(format: ImageFormat) -> ImageOptions {
    ImageOptions::new(format)
}

fn invalid_frame(result: crate::Result<impl std::fmt::Debug>) -> String {
    match result {
        Err(Error::InvalidFrame(message)) => message,
        other => panic!("expected InvalidFrame, got {other:?}"),
    }
}

fn invalid_input(result: crate::Result<impl std::fmt::Debug>) -> String {
    match result {
        Err(Error::InvalidInput(message)) => message,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Conversion against the reference
// ---------------------------------------------------------------------------

/// Encode `codes` as a PNG at `depth` with `policy` and check every pixel
/// against the reference, at most one code apart.
fn check_against_reference(
    codes: &Codes,
    matrix: YuvMatrix,
    range: YuvRange,
    depth: SampleDepth,
    policy: ColorPolicy,
    metadata: Option<&str>,
) {
    let frame = codes.frame(metadata);
    let png = frame
        .encode_image(&options(ImageFormat::Png).depth(depth).color(policy))
        .unwrap();
    let decoded = decode_png(&png);
    let max = if depth == SampleDepth::Eight {
        255.0
    } else {
        65535.0
    };
    assert_eq!(
        (
            decoded.width as usize,
            decoded.height as usize,
            decoded.channels
        ),
        (codes.width, codes.height, 4)
    );
    let bits = codes.sampling().bits;
    for y in 0..codes.height {
        for x in 0..codes.width {
            let [luma, cb, cr, alpha] = codes.pixel(x, y);
            let rgb = reference_rgb(matrix, range, bits, max, [luma, cb, cr]);
            let at = (y * codes.width + x) * 4;
            for (c, want) in rgb
                .into_iter()
                .chain([reference_alpha(alpha, bits, max)])
                .enumerate()
            {
                let got = f64::from(decoded.samples[at + c]);
                assert!(
                    (got - want).abs() <= 1.0,
                    "{:?} {matrix:?} {range:?} {depth:?} pixel {x},{y} channel {c}: {got} against {want} from {:?}",
                    codes.format,
                    codes.pixel(x, y)
                );
            }
        }
    }
}

#[test]
fn every_reader_matrix_range_and_depth_matches_the_reference() {
    for format in YUV_FORMATS {
        for codes in [
            boundary_codes(format, 64, 32),
            random_codes(format, 18, 6, 0xC0DE ^ format as u64),
        ] {
            for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
                for range in [YuvRange::Limited, YuvRange::Full] {
                    for depth in [SampleDepth::Eight, SampleDepth::Sixteen] {
                        check_against_reference(
                            &codes,
                            matrix,
                            range,
                            depth,
                            ColorPolicy::Override { matrix, range },
                            None,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_sdk_policy_takes_the_tagged_matrix_or_bt709_and_limited_range() {
    for format in [
        PixelFormat::Uyvy,
        PixelFormat::P216,
        PixelFormat::Nv12,
        PixelFormat::Nb4g,
    ] {
        let codes = random_codes(format, 16, 4, 0xFACE);
        for (metadata, matrix) in [
            (None, YuvMatrix::Bt709),
            (Some("<other/>"), YuvMatrix::Bt709),
            (
                Some("<ndi_color_info primaries=\"bt_601\"/>"),
                YuvMatrix::Bt709,
            ),
            (
                Some("<ndi_color_info matrix=\"bt_601\"/>"),
                YuvMatrix::Bt601,
            ),
            (
                Some("<ndi_color_info matrix=\"bt_709\"/>"),
                YuvMatrix::Bt709,
            ),
            (
                Some("<ndi_metadata_group><ndi_color_info matrix='bt_2020'/></ndi_metadata_group>"),
                YuvMatrix::Bt2020,
            ),
            (
                Some("<ndi_color_info matrix=\"bt_2100\" transfer=\"bt_709\"/>"),
                YuvMatrix::Bt2020,
            ),
        ] {
            for depth in [SampleDepth::Eight, SampleDepth::Sixteen] {
                check_against_reference(
                    &codes,
                    matrix,
                    YuvRange::Limited,
                    depth,
                    ColorPolicy::Sdk,
                    metadata,
                );
            }
        }
        // An override replaces the tag.
        check_against_reference(
            &codes,
            YuvMatrix::Bt601,
            YuvRange::Full,
            SampleDepth::Eight,
            ColorPolicy::Override {
                matrix: YuvMatrix::Bt601,
                range: YuvRange::Full,
            },
            Some("<ndi_color_info matrix=\"bt_2020\"/>"),
        );
    }
}

/// The forward matrix in `f64`: RGB at 8 bits to code values at the
/// source's `bits` bits, the inverse of the reference.
fn forward(matrix: YuvMatrix, range: YuvRange, bits: u32, [r, g, b]: [u8; 3]) -> [u16; 3] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        _ => (0.2627, 0.0593),
    };
    let [r, g, b] = [r, g, b].map(|v| f64::from(v) / 255.0);
    let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
    let pb = (b - luma) / (2.0 * (1.0 - kb));
    let pr = (r - luma) / (2.0 * (1.0 - kr));
    let max = f64::from((1u32 << bits) - 1);
    let codes = match range {
        YuvRange::Limited => {
            let scale = f64::from(1u32 << (bits - 8));
            [16.0 + 219.0 * luma, 128.0 + 224.0 * pb, 128.0 + 224.0 * pr].map(|v| v * scale)
        }
        _ => {
            let half = f64::from(1u32 << (bits - 1));
            [max * luma, half + max * pb, half + max * pr]
        }
    };
    codes.map(|v| v.round().clamp(0.0, max) as u16)
}

#[test]
fn flat_colors_survive_a_round_trip_through_every_layout() {
    let colors: [[u8; 3]; 12] = [
        [0, 0, 0],
        [255, 255, 255],
        [128, 128, 128],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [0, 255, 255],
        [255, 0, 255],
        [255, 255, 0],
        [64, 128, 192],
        [200, 100, 50],
        [30, 60, 90],
    ];
    let (width, height) = (8 * colors.len(), 8);
    for format in YUV_FORMATS {
        let bits = sampling(format).bits;
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            for range in [YuvRange::Limited, YuvRange::Full] {
                let codes = Codes::new(format, width, height, |site| {
                    let s = sampling(format);
                    let block = |x: usize| colors[x / 8];
                    match site {
                        Site::Luma(i) => forward(matrix, range, bits, block(i % width))[0],
                        Site::Cb(i) => forward(
                            matrix,
                            range,
                            bits,
                            block((i % (width >> s.h_shift)) << s.h_shift),
                        )[1],
                        Site::Cr(i) => forward(
                            matrix,
                            range,
                            bits,
                            block((i % (width >> s.h_shift)) << s.h_shift),
                        )[2],
                        Site::Alpha(_) => ((1u32 << bits) - 1) as u16,
                    }
                });
                let png = codes
                    .frame(None)
                    .encode_image(
                        &options(ImageFormat::Png)
                            .alpha(AlphaMode::Discard)
                            .color(ColorPolicy::Override { matrix, range }),
                    )
                    .unwrap();
                let decoded = decode_png(&png);
                assert_eq!(decoded.channels, 3);
                let tolerance = if bits == 8 { 2 } else { 1 };
                for (i, pixel) in decoded.samples.as_chunks::<3>().0.iter().enumerate() {
                    let want = colors[(i % width) / 8];
                    for (got, want) in pixel.iter().zip(want) {
                        assert!(
                            got.abs_diff(u16::from(want)) <= tolerance,
                            "{format:?} {matrix:?} {range:?} pixel {i}: {pixel:?} against {want:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_chroma_impulse_covers_exactly_the_pixels_it_is_sited_on() {
    let neutral = |bits: u32| 128u16 << (bits - 8);
    for format in YUV_FORMATS {
        let s = sampling(format);
        let (width, height) = (16, 8);
        let (cw, ch) = (width >> s.h_shift, height >> s.v_shift);
        let impulse = (ch / 2) * cw + cw / 2;
        let codes = Codes::new(format, width, height, |site| match site {
            Site::Luma(_) => 126 << (s.bits - 8),
            Site::Alpha(_) => ((1u32 << s.bits) - 1) as u16,
            Site::Cb(i) | Site::Cr(i) if i == impulse => 200 << (s.bits - 8),
            Site::Cb(_) | Site::Cr(_) => neutral(s.bits),
        });
        let decoded = decode_png(&codes.frame(None).encode_png().unwrap());
        let gray = &decoded.samples[..4];
        let changed: Vec<(usize, usize)> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| &decoded.samples[(y * width + x) * 4..][..4] != gray)
            .collect();
        let (cx, cy) = (impulse % cw, impulse / cw);
        let expected: Vec<(usize, usize)> = (cy << s.v_shift..(cy + 1) << s.v_shift)
            .flat_map(|y| (cx << s.h_shift..(cx + 1) << s.h_shift).map(move |x| (x, y)))
            .collect();
        assert_eq!(changed, expected, "{format:?}");
    }
}

// ---------------------------------------------------------------------------
// Layouts and frame types
// ---------------------------------------------------------------------------

/// Every way an image can be asked for, for comparisons between layouts.
fn every_output() -> Vec<ImageOptions> {
    vec![
        options(ImageFormat::Png),
        options(ImageFormat::Png).depth(SampleDepth::Sixteen),
        options(ImageFormat::Png).alpha(AlphaMode::Discard),
        options(ImageFormat::Png).downscale(Downscale::Half),
        options(ImageFormat::Jpeg(85)),
        options(ImageFormat::Jpeg(90)).downscale(Downscale::Quarter),
        options(ImageFormat::WebpLossless),
        options(ImageFormat::WebpLossless).alpha(AlphaMode::Discard),
        options(ImageFormat::WebpLossy(75)),
        options(ImageFormat::WebpLossy(90))
            .alpha(AlphaMode::Discard)
            .downscale(Downscale::Quarter),
    ]
}

fn rgb_bytes(width: usize, height: usize, seed: u64) -> Vec<u8> {
    let mut random = XorShift(seed);
    (0..width * height * 4)
        .map(|_| random.next() as u8)
        .collect()
}

/// The packed bytes of a frame of `format`.
fn any_frame_bytes(format: PixelFormat, width: usize, height: usize, seed: u64) -> Vec<u8> {
    if RGB_FORMATS.contains(&format) {
        rgb_bytes(width, height, seed)
    } else {
        random_codes(format, width, height, seed).bytes()
    }
}

/// `packed` laid out with `padding` bytes after each row of the first
/// plane, and the matching padding in the others, as the SDK pads. The
/// padding bytes are 0xEE.
fn padded(
    format: PixelFormat,
    width: u32,
    height: u32,
    packed: &[u8],
    padding: usize,
) -> VideoFrame {
    let packed = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(format)
        .data(packed.to_vec())
        .build()
        .unwrap();
    let stride = packed.line_stride_bytes() + padding;
    let len = format.info().buffer_len(stride, height).unwrap();
    let mut frame =
        VideoFrame::with_line_stride(format, width, height, stride, vec![0xEE; len], None);
    let base = frame.data().as_ptr() as usize;
    let starts: Vec<usize> = frame
        .planes()
        .flat_map(|plane| plane.rows())
        .map(|row| row.as_ptr() as usize - base)
        .collect();
    for (start, row) in starts
        .into_iter()
        .zip(packed.planes().flat_map(|plane| plane.rows()))
    {
        frame.data_mut()[start..start + row.len()].copy_from_slice(row);
    }
    frame
}

#[test]
fn padded_rows_odd_heights_and_the_smallest_frames_encode_like_packed_ones() {
    for format in PixelFormat::ALL {
        let four_twenty = matches!(
            format,
            PixelFormat::Nv12 | PixelFormat::I420 | PixelFormat::Yv12
        );
        let sizes: &[(u32, u32)] = if four_twenty {
            &[(2, 2), (18, 6), (34, 18)]
        } else {
            &[(2, 1), (4, 3), (18, 5), (34, 17)]
        };
        for &(width, height) in sizes {
            let packed = any_frame_bytes(format, width as usize, height as usize, 99);
            let frame = VideoFrame::builder()
                .resolution(width, height)
                .pixel_format(format)
                .data(packed.clone())
                .build()
                .unwrap();
            for padding in [4, 64] {
                let padded = padded(format, width, height, &packed, padding);
                assert!(padded.line_stride_bytes() > frame.line_stride_bytes());
                for options in every_output() {
                    assert_eq!(
                        padded.encode_image(&options).unwrap(),
                        frame.encode_image(&options).unwrap(),
                        "{format:?} {width}x{height} padding {padding} {options:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn every_frame_type_encodes_the_same_bytes() {
    for format in PixelFormat::ALL {
        let (width, height) = (18u32, 6u32);
        let packed = any_frame_bytes(format, width as usize, height as usize, 5);
        let owned = VideoFrame::builder()
            .resolution(width, height)
            .pixel_format(format)
            .data(packed.clone())
            .metadata("<ndi_color_info matrix=\"bt_601\"/>")
            .build()
            .unwrap();
        let padded = padded(format, width, height, &packed, 8);
        let captured = owned.as_test_capture();
        let borrowed = BorrowedVideoFrame::builder(&packed, width, height, format)
            .metadata(c"<ndi_color_info matrix=\"bt_601\"/>")
            .build()
            .unwrap();
        for options in every_output() {
            let expected = owned.encode_image(&options).unwrap();
            assert_eq!(
                captured.encode_image(&options).unwrap(),
                expected,
                "{format:?} {options:?}"
            );
            assert_eq!(
                borrowed.encode_image(&options).unwrap(),
                expected,
                "{format:?} {options:?}"
            );
            let mut reused = vec![1, 2, 3];
            reused.clear();
            borrowed.encode_image_to(&options, &mut reused).unwrap();
            assert_eq!(reused, expected);
            // The padded copy has no metadata, so only RGB frames, which
            // the matrix does not touch, agree with it.
            if RGB_FORMATS.contains(&format) {
                assert_eq!(
                    padded.as_test_capture().encode_image(&options).unwrap(),
                    expected
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The four RGB formats keep 2.0's bytes
// ---------------------------------------------------------------------------

/// SHA-256 of grafton-ndi 2.0's output for the RGB formats, recorded from the
/// 2.0 encoder before it was replaced: each covers sizes 2×1, 2×2, 18×19,
/// 64×33 and 130×67, packed and padded rows, noise and gradient content.
const IDENTITY_HASHES: [(PixelFormat, Option<u8>, &str); 28] = [
    (
        PixelFormat::Rgba,
        None,
        "8b6a1a6fa0442ac9a2e7e1fdfc8608d1447d42cd48497325238f7f04fb60ef2b",
    ),
    (
        PixelFormat::Rgba,
        Some(1),
        "6a95d2393cc79d5ee1d8379533b6104d2ca3c375ab3fafdd8680751dfe7084af",
    ),
    (
        PixelFormat::Rgba,
        Some(50),
        "dbd3d139edea67c7e5815eb2cb9f8695690384a060300f916b445ebbdf65a9ff",
    ),
    (
        PixelFormat::Rgba,
        Some(85),
        "26ad91537bd9d6ef1bdf89c75dab1006ddf168956b41501aba4a606bcc4cafe7",
    ),
    (
        PixelFormat::Rgba,
        Some(89),
        "c49592df5120fe1feb20217a6104f3cd5978c2ef85972acbc9fe27ee2e22c095",
    ),
    (
        PixelFormat::Rgba,
        Some(90),
        "0e9ef62103266806fcb4a5bd7b4bfd0fdd6891a475898200897a2e6b489ee20b",
    ),
    (
        PixelFormat::Rgba,
        Some(100),
        "7fe741321490a6edc60d2bb621e95b46f7f9ba16fc26dec78e9842de0249e4b0",
    ),
    (
        PixelFormat::Bgra,
        None,
        "03c0670be8d0ff1f0bc147acfc6ba1867ca34c338ceb52fb99872b941b69d2aa",
    ),
    (
        PixelFormat::Bgra,
        Some(1),
        "9c10d54f1c27feccc21cfc2cfea76132022e421f090b55ad03e89a735ea184e9",
    ),
    (
        PixelFormat::Bgra,
        Some(50),
        "429ddce45abdd489698b12bf82a6912f9fce39499b7be9062fff66e9e4d99c27",
    ),
    (
        PixelFormat::Bgra,
        Some(85),
        "ec80964faa9e0840f07cbe0638c9e66cb6555325584f004fee87a7bf6f523fee",
    ),
    (
        PixelFormat::Bgra,
        Some(89),
        "492f9505d0de7467ab9b98d3760058d72af85df4c62b719daf6c97a346cc7c9b",
    ),
    (
        PixelFormat::Bgra,
        Some(90),
        "a879797d164545e7e65edabe2c924dd7911b66087361f32373172ccb3b782c6a",
    ),
    (
        PixelFormat::Bgra,
        Some(100),
        "8727a6a2ef622d4f0758b96cf9a10b49567ce82e6abefc29c5bcf8fffd5c89f0",
    ),
    (
        PixelFormat::Rgbx,
        None,
        "eb701500fdaaafbd0b77f6bd736a3c6a78c5fa7188234a6fb9bb2f6b8595add4",
    ),
    (
        PixelFormat::Rgbx,
        Some(1),
        "6a95d2393cc79d5ee1d8379533b6104d2ca3c375ab3fafdd8680751dfe7084af",
    ),
    (
        PixelFormat::Rgbx,
        Some(50),
        "dbd3d139edea67c7e5815eb2cb9f8695690384a060300f916b445ebbdf65a9ff",
    ),
    (
        PixelFormat::Rgbx,
        Some(85),
        "26ad91537bd9d6ef1bdf89c75dab1006ddf168956b41501aba4a606bcc4cafe7",
    ),
    (
        PixelFormat::Rgbx,
        Some(89),
        "c49592df5120fe1feb20217a6104f3cd5978c2ef85972acbc9fe27ee2e22c095",
    ),
    (
        PixelFormat::Rgbx,
        Some(90),
        "0e9ef62103266806fcb4a5bd7b4bfd0fdd6891a475898200897a2e6b489ee20b",
    ),
    (
        PixelFormat::Rgbx,
        Some(100),
        "7fe741321490a6edc60d2bb621e95b46f7f9ba16fc26dec78e9842de0249e4b0",
    ),
    (
        PixelFormat::Bgrx,
        None,
        "ba2edbd6b06426db017932c336c0a34eef4f85ee35b6690cfa42d7061de7f91a",
    ),
    (
        PixelFormat::Bgrx,
        Some(1),
        "9c10d54f1c27feccc21cfc2cfea76132022e421f090b55ad03e89a735ea184e9",
    ),
    (
        PixelFormat::Bgrx,
        Some(50),
        "429ddce45abdd489698b12bf82a6912f9fce39499b7be9062fff66e9e4d99c27",
    ),
    (
        PixelFormat::Bgrx,
        Some(85),
        "ec80964faa9e0840f07cbe0638c9e66cb6555325584f004fee87a7bf6f523fee",
    ),
    (
        PixelFormat::Bgrx,
        Some(89),
        "492f9505d0de7467ab9b98d3760058d72af85df4c62b719daf6c97a346cc7c9b",
    ),
    (
        PixelFormat::Bgrx,
        Some(90),
        "a879797d164545e7e65edabe2c924dd7911b66087361f32373172ccb3b782c6a",
    ),
    (
        PixelFormat::Bgrx,
        Some(100),
        "8727a6a2ef622d4f0758b96cf9a10b49567ce82e6abefc29c5bcf8fffd5c89f0",
    ),
];

/// The inputs the hashes were recorded from, in recording order.
fn identity_frames(format: PixelFormat) -> Vec<VideoFrame> {
    let mut frames = Vec::new();
    for (width, height) in [(2u32, 1u32), (2, 2), (18, 19), (64, 33), (130, 67)] {
        for padding in [0usize, 12] {
            for noise in [true, false] {
                let (w, h) = (width as usize, height as usize);
                let stride = w * 4 + padding;
                let mut data = vec![0xCD; stride * h];
                let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ (w * 1000 + h) as u64;
                for y in 0..h {
                    for x in 0..w {
                        let pixel = &mut data[y * stride + x * 4..][..4];
                        if noise {
                            for byte in pixel {
                                seed ^= seed << 13;
                                seed ^= seed >> 7;
                                seed ^= seed << 17;
                                *byte = seed as u8;
                            }
                        } else {
                            pixel.copy_from_slice(&[
                                (x * 255 / (w - 1)) as u8,
                                (y * 255 / (h - 1).max(1)) as u8,
                                ((x + y) * 7) as u8,
                                (x * 3 + y * 5) as u8,
                            ]);
                        }
                    }
                }
                frames.push(VideoFrame::with_line_stride(
                    format, width, height, stride, data, None,
                ));
            }
        }
    }
    frames
}

fn identity_hash(images: impl Iterator<Item = Vec<u8>>) -> String {
    let mut all = Vec::new();
    for image in images {
        all.extend_from_slice(&(image.len() as u64).to_le_bytes());
        all.extend_from_slice(&image);
    }
    sha256(&all)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn rgb_formats_keep_the_bytes_of_grafton_ndi_2() {
    for (format, quality, expected) in IDENTITY_HASHES {
        let frames = identity_frames(format);
        let shortcut = identity_hash(frames.iter().map(|frame| match quality {
            None => frame.encode_png().unwrap(),
            Some(quality) => frame.encode_jpeg(quality).unwrap(),
        }));
        assert_eq!(shortcut, expected, "{format:?} {quality:?} shortcut");
        let options = ImageOptions::new(quality.map_or(ImageFormat::Png, ImageFormat::Jpeg));
        let mut buffer = Vec::new();
        let streamed = identity_hash(frames.iter().map(|frame| {
            buffer.clear();
            frame
                .as_test_capture()
                .encode_image_to(&options, &mut buffer)
                .unwrap();
            buffer.clone()
        }));
        assert_eq!(
            streamed, expected,
            "{format:?} {quality:?} streamed from a capture"
        );
        // An SDR tag changes nothing.
        let tagged = identity_hash(frames.iter().map(|frame| {
            let tagged = VideoFrame::with_line_stride(
                format,
                frame.width(),
                frame.height(),
                frame.line_stride_bytes(),
                frame.data().to_vec(),
                Some(
                    "<ndi_color_info transfer=\"bt_709\" matrix=\"bt_709\" primaries=\"bt_709\"/>",
                ),
            );
            tagged.encode_image(&options).unwrap()
        }));
        assert_eq!(tagged, expected, "{format:?} {quality:?} SDR-tagged");
    }
}

#[test]
fn sha256_matches_known_vectors() {
    let hex = |digest: [u8; 32]| {
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    assert_eq!(
        hex(sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(sha256(&[b'a'; 1000])),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

/// SHA-256 (FIPS 180-4), for the identity hashes, which `sha256sum` can
/// check.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (k, w) in K.iter().zip(w) {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(*k)
                .wrapping_add(w);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
    let mut digest = [0; 32];
    for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *bytes = word.to_be_bytes();
    }
    digest
}

// ---------------------------------------------------------------------------
// Codecs: exact decodes, PNG levels, data URLs
// ---------------------------------------------------------------------------

#[test]
fn every_png_level_and_lossless_webp_decode_to_the_converted_pixels() {
    for format in PixelFormat::ALL {
        let (width, height) = (34u32, 10u32);
        let packed = any_frame_bytes(format, width as usize, height as usize, 21);
        for frame in [
            VideoFrame::builder()
                .resolution(width, height)
                .pixel_format(format)
                .data(packed.clone())
                .build()
                .unwrap(),
            padded(format, width, height, &packed, 8),
        ] {
            for alpha in [AlphaMode::Keep, AlphaMode::Discard] {
                let reference = decode_png(
                    &frame
                        .encode_image(&options(ImageFormat::Png).alpha(alpha))
                        .unwrap(),
                );
                assert_eq!(
                    reference.channels,
                    if alpha == AlphaMode::Keep { 4 } else { 3 }
                );
                for level in [
                    PngCompression::Fast,
                    PngCompression::Balanced,
                    PngCompression::High,
                ] {
                    let png = frame
                        .encode_image(
                            &options(ImageFormat::Png)
                                .alpha(alpha)
                                .png_compression(level),
                        )
                        .unwrap();
                    assert_eq!(
                        decode_png(&png),
                        reference,
                        "{format:?} {alpha:?} {level:?}"
                    );
                }
                let webp = frame
                    .encode_image(&options(ImageFormat::WebpLossless).alpha(alpha))
                    .unwrap();
                let decoded = decode_webp(&webp);
                // WebP decoders report alpha only when it is used.
                let expected: Vec<u16> = if decoded.channels == reference.channels {
                    reference.samples.clone()
                } else {
                    reference
                        .samples
                        .chunks_exact(reference.channels)
                        .flat_map(|pixel| pixel[..3].to_vec())
                        .collect()
                };
                assert_eq!((decoded.width, decoded.height), (width, height));
                assert_eq!(decoded.samples, expected, "{format:?} {alpha:?} WebP");
            }
        }
    }
    // An RGBA frame's own bytes, read in place, decode back to themselves.
    let rgba = rgb_bytes(34, 10, 3);
    let frame = VideoFrame::builder()
        .resolution(34, 10)
        .pixel_format(PixelFormat::Rgba)
        .data(rgba.clone())
        .build()
        .unwrap();
    let decoded = decode_webp(
        &frame
            .encode_image(&options(ImageFormat::WebpLossless))
            .unwrap(),
    );
    assert_eq!(
        decoded.samples,
        rgba.iter().map(|&v| u16::from(v)).collect::<Vec<_>>()
    );
}

#[test]
fn lossy_webp_reduces_every_pixel_format_and_preserves_alpha() {
    for format in PixelFormat::ALL {
        let frame = VideoFrame::builder()
            .resolution(18, 10)
            .pixel_format(format)
            .data(any_frame_bytes(format, 18, 10, 21))
            .build()
            .unwrap();
        for alpha in [AlphaMode::Keep, AlphaMode::Discard] {
            for downscale in [
                Downscale::Off,
                Downscale::Half,
                Downscale::Quarter,
                Downscale::Eighth,
            ] {
                let reference = decode_png(
                    &frame
                        .encode_image(&options(ImageFormat::Png).alpha(alpha).downscale(downscale))
                        .unwrap(),
                );
                let rgba: Vec<u8> = reference
                    .samples
                    .chunks_exact(reference.channels)
                    .flat_map(|p| {
                        [
                            p[0] as u8,
                            p[1] as u8,
                            p[2] as u8,
                            if reference.channels == 4 {
                                p[3] as u8
                            } else {
                                255
                            },
                        ]
                    })
                    .collect();
                let image = frame
                    .encode_image(
                        &options(ImageFormat::WebpLossy(75))
                            .alpha(alpha)
                            .downscale(downscale),
                    )
                    .unwrap();
                let decoded = decode_webp(&image);
                assert_eq!(
                    (decoded.width, decoded.height),
                    (reference.width, reference.height)
                );
                let transparent = rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
                assert_eq!(decoded.channels, if transparent { 4 } else { 3 });
                if transparent {
                    let actual: Vec<u16> = decoded
                        .samples
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| p[3])
                        .collect();
                    let expected: Vec<u16> = rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| u16::from(p[3]))
                        .collect();
                    assert_eq!(actual, expected, "{format:?} {downscale:?} alpha");
                }
            }
        }
    }
}

#[test]
fn lossy_webp_effort_and_color_conversion_preserve_alpha() {
    let (width, height) = (64, 48);
    let mut rgba = content_rgba("photographic", width, height, None);
    for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        pixel[3] = (i % 256) as u8;
    }
    let expected_alpha: Vec<_> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| u16::from(pixel[3]))
        .collect();
    let frame = frame_from_rgba(PixelFormat::Rgba, width, height, &rgba);
    let defaults = options(ImageFormat::WebpLossy(75));
    let default_image = frame.encode_image(&defaults).unwrap();
    assert_eq!(
        default_image,
        frame
            .encode_image(&defaults.clone().webp_effort(4).webp_sharp_yuv(true))
            .unwrap()
    );
    assert_ne!(
        default_image,
        frame
            .encode_image(&defaults.clone().webp_effort(0))
            .unwrap()
    );
    assert_ne!(
        default_image,
        frame
            .encode_image(&defaults.clone().webp_sharp_yuv(false))
            .unwrap()
    );
    for effort in 0..=6 {
        for sharp_yuv in [false, true] {
            let options = defaults
                .clone()
                .webp_effort(effort)
                .webp_sharp_yuv(sharp_yuv);
            let image = frame.encode_image(&options).unwrap();
            let mut streamed = Vec::new();
            frame.encode_image_to(&options, &mut streamed).unwrap();
            assert_eq!(image, streamed);
            let decoded = decode_webp(&image);
            assert_eq!(
                (decoded.width, decoded.height, decoded.channels),
                (width as u32, height as u32, 4)
            );
            let alpha: Vec<_> = decoded
                .samples
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3])
                .collect();
            assert_eq!(
                alpha, expected_alpha,
                "effort {effort}, SharpYUV {sharp_yuv}"
            );
        }
    }
}

#[test]
fn lossy_webp_quality_controls_size_and_color_fidelity() {
    let (width, height) = (128, 96);
    let rgba = content_rgba("photographic", width, height, None);
    let frame = frame_from_rgba(PixelFormat::Rgba, width, height, &rgba);
    let rgb: Vec<u16> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| p[..3].iter().copied().map(u16::from))
        .collect();
    let mut measurements = Vec::new();
    for quality in [0, 1, 50, 75, 100] {
        let options = options(ImageFormat::WebpLossy(quality));
        let image = frame.encode_image(&options).unwrap();
        assert!(image_webp::WebPDecoder::new(std::io::Cursor::new(&image))
            .unwrap()
            .is_lossy());
        let mut streamed = Vec::new();
        frame.encode_image_to(&options, &mut streamed).unwrap();
        assert_eq!(image, streamed);
        let decoded = decode_webp(&image);
        assert_eq!(
            (decoded.width, decoded.height, decoded.channels),
            (width as u32, height as u32, 3)
        );
        let error: u64 = decoded
            .samples
            .iter()
            .zip(&rgb)
            .map(|(&a, &b)| {
                let difference = i64::from(a) - i64::from(b);
                (difference * difference) as u64
            })
            .sum();
        measurements.push((image.len(), error));
    }
    let (small, inaccurate) = measurements[0];
    let (large, accurate) = measurements[4];
    assert!(small < large && accurate < inaccurate, "{measurements:?}");
    assert!(accurate > 0, "quality 100 is still lossy");
    assert!(
        small
            < frame
                .encode_image(&options(ImageFormat::WebpLossless))
                .unwrap()
                .len()
    );
}

#[test]
fn data_urls_carry_the_image_and_are_allocated_once() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let frame = random_codes(PixelFormat::Uyvy, 18, 6, 1).frame(None);
    for (format, prefix) in [
        (ImageFormat::Png, "data:image/png;base64,"),
        (ImageFormat::Jpeg(70), "data:image/jpeg;base64,"),
        (ImageFormat::WebpLossless, "data:image/webp;base64,"),
        (ImageFormat::WebpLossy(75), "data:image/webp;base64,"),
    ] {
        let url = frame.encode_image_data_url(&options(format)).unwrap();
        assert_eq!(url.capacity(), url.len());
        let image = STANDARD.decode(url.strip_prefix(prefix).unwrap()).unwrap();
        assert_eq!(image, frame.encode_image(&options(format)).unwrap());
        assert_eq!(frame.encode_data_url(format).unwrap(), url);
    }
}

#[test]
fn sixteen_bit_png_keeps_high_bit_depth_precision() {
    // Codes that differ only in their low byte stay distinct at 16 bits.
    let codes = Codes::new(PixelFormat::P216, 4, 1, |site| match site {
        Site::Luma(i) => 0x8000 + i as u16,
        _ => 0x8000,
    });
    let frame = codes.frame(None);
    let decoded = decode_png(
        &frame
            .encode_image(&options(ImageFormat::Png).depth(SampleDepth::Sixteen))
            .unwrap(),
    );
    assert_eq!(decoded.bits, 16);
    let greens: Vec<u16> = decoded
        .samples
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[1])
        .collect();
    assert!(
        greens.windows(2).all(|pair| pair[0] < pair[1]),
        "{greens:?}"
    );
    // An 8-bit source scales exactly.
    let rgba = VideoFrame::builder()
        .resolution(2, 1)
        .pixel_format(PixelFormat::Rgba)
        .data(vec![0, 1, 254, 255, 128, 129, 2, 3])
        .build()
        .unwrap();
    let decoded = decode_png(
        &rgba
            .encode_image(&options(ImageFormat::Png).depth(SampleDepth::Sixteen))
            .unwrap(),
    );
    assert_eq!(
        decoded.samples,
        [
            0,
            257,
            254 * 257,
            65535,
            128 * 257,
            129 * 257,
            2 * 257,
            3 * 257
        ]
    );
}

// ---------------------------------------------------------------------------
// HDR and cICP
// ---------------------------------------------------------------------------

#[test]
fn hdr_frames_are_refused_by_default_in_every_format() {
    for format in PixelFormat::ALL {
        for (transfer, name) in [("bt_2100_pq", "PQ"), ("bt_2100_hlg", "HLG")] {
            let metadata = format!("<ndi_color_info transfer=\"{transfer}\" primaries=\"bt_2100\" matrix=\"bt_2100\"/>");
            let frame = VideoFrame::builder()
                .resolution(2, 2)
                .pixel_format(format)
                .metadata(metadata.as_str())
                .build()
                .unwrap();
            for shortcut in [
                frame.encode_png(),
                frame.encode_jpeg(85),
                frame.encode_image(&options(ImageFormat::WebpLossless)),
                frame.encode_image(&options(ImageFormat::WebpLossy(75))),
            ] {
                let message = invalid_frame(shortcut);
                assert!(
                    message.contains(name) && message.contains("HdrPolicy::CodeValues"),
                    "{message}"
                );
            }
            let hdr = |format| options(format).hdr(HdrPolicy::CodeValues);
            for refused in [
                ImageFormat::Jpeg(85),
                ImageFormat::WebpLossless,
                ImageFormat::WebpLossy(75),
            ] {
                let message = invalid_input(frame.encode_image(&hdr(refused)));
                assert!(
                    message.contains(name) && message.contains("ImageFormat::Png"),
                    "{message}"
                );
            }
            let png = frame.encode_image(&hdr(ImageFormat::Png)).unwrap();
            let chunks = png_chunks(&png);
            let cicp = chunks.iter().find(|(kind, _)| kind == b"cICP").unwrap();
            assert_eq!(cicp.1, [9, if name == "PQ" { 16 } else { 18 }, 0, 1]);
            // The readable tag still decides under an override.
            let overridden = options(ImageFormat::Png).color(ColorPolicy::Override {
                matrix: YuvMatrix::Bt709,
                range: YuvRange::Limited,
            });
            invalid_frame(frame.encode_image(&overridden));
        }
    }
}

#[test]
fn cicp_tags_bt2020_and_hdr_pngs_before_their_image_data() {
    for (metadata, hdr, expected) in [
        (None, HdrPolicy::Refuse, None),
        (
            Some("<ndi_color_info primaries=\"bt_709\" transfer=\"bt_709\"/>"),
            HdrPolicy::Refuse,
            None,
        ),
        (
            Some("<ndi_color_info primaries=\"bt_601\"/>"),
            HdrPolicy::Refuse,
            None,
        ),
        (
            Some("<ndi_color_info transfer=\"bt_2020\"/>"),
            HdrPolicy::Refuse,
            None,
        ),
        (
            Some("<ndi_color_info primaries=\"bt_2020\"/>"),
            HdrPolicy::Refuse,
            Some([9, 1, 0, 1]),
        ),
        (
            Some("<ndi_color_info primaries=\"bt_2100\" transfer=\"bt_2020\"/>"),
            HdrPolicy::Refuse,
            Some([9, 1, 0, 1]),
        ),
        (
            Some("<ndi_color_info primaries=\"bt_2020\" transfer=\"bt_2100_pq\"/>"),
            HdrPolicy::CodeValues,
            Some([9, 16, 0, 1]),
        ),
        (
            Some("<ndi_color_info primaries=\"bt_709\" transfer=\"bt_2100_hlg\"/>"),
            HdrPolicy::CodeValues,
            Some([1, 18, 0, 1]),
        ),
        (
            Some("<ndi_color_info primaries=\"bt_601\" transfer=\"bt_2100_pq\"/>"),
            HdrPolicy::CodeValues,
            Some([6, 16, 0, 1]),
        ),
        (
            Some("<ndi_color_info transfer=\"bt_2100_hlg\"/>"),
            HdrPolicy::CodeValues,
            Some([2, 18, 0, 1]),
        ),
    ] {
        for (format, depth) in [
            (PixelFormat::P216, SampleDepth::Sixteen),
            (PixelFormat::Uyvy, SampleDepth::Eight),
            (PixelFormat::Rgbx, SampleDepth::Eight),
        ] {
            let mut builder = VideoFrame::builder().resolution(4, 2).pixel_format(format);
            if let Some(metadata) = metadata {
                builder = builder.metadata(metadata);
            }
            let frame = builder.build().unwrap();
            let png = frame
                .encode_image(&options(ImageFormat::Png).depth(depth).hdr(hdr))
                .unwrap();
            let chunks = png_chunks(&png);
            let kinds: Vec<[u8; 4]> = chunks.iter().map(|(kind, _)| *kind).collect();
            let cicp = kinds.iter().position(|kind| kind == b"cICP");
            match expected {
                None => assert_eq!(cicp, None, "{metadata:?}"),
                Some(bytes) => {
                    let at = cicp.unwrap();
                    assert_eq!(chunks[at].1, bytes, "{metadata:?}");
                    assert_eq!(kinds[0], *b"IHDR");
                    assert!(at < kinds.iter().position(|kind| kind == b"IDAT").unwrap());
                    let reader = ::png::Decoder::new(std::io::Cursor::new(&png))
                        .read_info()
                        .unwrap();
                    let read = reader.info().coding_independent_code_points.unwrap();
                    assert_eq!(
                        [
                            read.color_primaries,
                            read.transfer_function,
                            read.matrix_coefficients,
                            u8::from(read.is_video_full_range_image)
                        ],
                        bytes
                    );
                }
            }
        }
    }
}

#[test]
fn processing_instructions_do_not_change_image_color_or_hdr_policy() {
    for format in [PixelFormat::Uyvy, PixelFormat::Rgba] {
        let mut frame = VideoFrame::builder()
            .resolution(2, 2)
            .pixel_format(format)
            .build()
            .unwrap();
        let formats = [
            ImageFormat::Png,
            ImageFormat::Jpeg(90),
            ImageFormat::WebpLossless,
            ImageFormat::WebpLossy(75),
        ];
        let expected = formats.map(|format| frame.encode_image(&options(format)).unwrap());
        for metadata in [
            "<ndi_metadata_group><?camera <ndi_color_info transfer='bt_2100_pq'/> ?></ndi_metadata_group>",
            "<ndi_metadata_group><?camera <ndi_color_info matrix='unknown'/> ?></ndi_metadata_group>",
        ] {
            frame.set_metadata(metadata).unwrap();
            for (format, expected) in formats.into_iter().zip(&expected) {
                assert_eq!(frame.encode_image(&options(format)).unwrap(), *expected);
            }
            assert_eq!(frame.color_info().unwrap(), None);
        }
    }
}

#[test]
fn unreadable_color_metadata_is_refused_unless_the_caller_overrides() {
    for format in [PixelFormat::Uyvy, PixelFormat::Rgba] {
        let frame = VideoFrame::builder()
            .resolution(2, 2)
            .pixel_format(format)
            .metadata("<ndi_color_info matrix=\"bt_9000\"/>")
            .build()
            .unwrap();
        assert!(frame.color_info().is_err());
        let message = invalid_frame(frame.encode_png());
        assert!(
            message.contains("matrix") && message.contains("bt_9000"),
            "{message}"
        );
        let overridden = options(ImageFormat::Png).color(ColorPolicy::Override {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited,
        });
        let png = frame.encode_image(&overridden).unwrap();
        assert!(png_chunks(&png).iter().all(|(kind, _)| kind != b"cICP"));
    }
}

// ---------------------------------------------------------------------------
// Option checks, sizes and writers
// ---------------------------------------------------------------------------

/// The error a [`Recorder`] fails with, which the caller must get back as
/// it is.
#[derive(Debug)]
struct DiskFull;

impl std::fmt::Display for DiskFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("disk full")
    }
}

impl std::error::Error for DiskFull {}

/// A writer that counts what it is given and can fail after a limit.
struct Recorder {
    written: usize,
    fail_after: Option<usize>,
}

impl std::io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self
            .fail_after
            .is_some_and(|limit| self.written + buf.len() > limit)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                DiskFull,
            ));
        }
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn conflicting_options_are_refused_before_anything_is_written() {
    let frame = random_codes(PixelFormat::P216, 4, 2, 1).frame(None);
    let cases = [
        (options(ImageFormat::Jpeg(0)), "quality"),
        (options(ImageFormat::Jpeg(101)), "quality"),
        (options(ImageFormat::Jpeg(255)), "quality"),
        (options(ImageFormat::WebpLossy(101)), "WebP quality"),
        (options(ImageFormat::WebpLossy(255)), "WebP quality"),
        (
            options(ImageFormat::WebpLossy(75)).webp_effort(7),
            "WebP effort",
        ),
        (
            options(ImageFormat::WebpLossy(75)).webp_effort(255),
            "WebP effort",
        ),
        (
            options(ImageFormat::Jpeg(85)).depth(SampleDepth::Sixteen),
            "SampleDepth::Sixteen",
        ),
        (
            options(ImageFormat::WebpLossless).depth(SampleDepth::Sixteen),
            "SampleDepth::Sixteen",
        ),
        (
            options(ImageFormat::WebpLossy(75)).depth(SampleDepth::Sixteen),
            "SampleDepth::Sixteen",
        ),
    ];
    for (options, needle) in cases {
        let mut out = Recorder {
            written: 0,
            fail_after: None,
        };
        let message = invalid_input(frame.encode_image_to(&options, &mut out));
        assert!(message.contains(needle), "{message}");
        assert_eq!(out.written, 0, "{options:?}");
    }
    for quality in [0, 101] {
        assert!(invalid_input(frame.encode_jpeg(quality)).contains("quality"));
        assert!(
            invalid_input(frame.encode_data_url(ImageFormat::Jpeg(quality))).contains("quality")
        );
    }
    // Quality is checked before the frame: even an HDR frame reports it.
    let hdr = VideoFrame::builder()
        .resolution(2, 2)
        .metadata("<ndi_color_info transfer=\"bt_2100_pq\"/>")
        .build()
        .unwrap();
    invalid_input(hdr.encode_jpeg(0));
    invalid_input(hdr.encode_image(&options(ImageFormat::WebpLossy(101))));
    invalid_input(hdr.encode_image(&options(ImageFormat::WebpLossy(75)).webp_effort(7)));
    // Codec-specific settings remain available when switching formats.
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg(85),
        ImageFormat::WebpLossless,
    ] {
        frame
            .encode_image(&options(format).webp_effort(255).webp_sharp_yuv(false))
            .unwrap();
    }
    // The PNG compression level is no error for JPEG or WebP.
    for format in [
        ImageFormat::Jpeg(85),
        ImageFormat::WebpLossless,
        ImageFormat::WebpLossy(75),
    ] {
        frame
            .encode_image(&options(format).png_compression(PngCompression::High))
            .unwrap();
    }
}

#[test]
fn images_larger_than_their_codec_allows_are_refused_naming_the_dimension() {
    let wide = VideoFrame::builder()
        .resolution(65_536, 2)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();
    assert!(invalid_frame(wide.encode_jpeg(85)).contains("JPEG width 65536"));
    // Reduced, it fits.
    let jpeg = wide
        .encode_image(&options(ImageFormat::Jpeg(85)).downscale(Downscale::Half))
        .unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let tall = VideoFrame::builder()
        .resolution(2, 65_536)
        .pixel_format(PixelFormat::Nv12)
        .build()
        .unwrap();
    assert!(invalid_frame(tall.encode_jpeg(85)).contains("JPEG height 65536"));
    let webp = VideoFrame::builder()
        .resolution(16_386, 1)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();
    assert!(
        invalid_frame(webp.encode_image(&options(ImageFormat::WebpLossless)))
            .contains("WebP width 16386")
    );
    webp.encode_image(&options(ImageFormat::WebpLossless).downscale(Downscale::Half))
        .unwrap();
    // Lossless permits 16384; VP8's 14-bit dimension stops at 16383.
    for (width, height, dimension) in [(16_384, 1, "width"), (2, 16_384, "height")] {
        let frame = VideoFrame::builder()
            .resolution(width, height)
            .build()
            .unwrap();
        let mut out = Recorder {
            written: 0,
            fail_after: None,
        };
        let message =
            invalid_frame(frame.encode_image_to(&options(ImageFormat::WebpLossy(75)), &mut out));
        assert!(
            message.contains(&format!("WebP {dimension} 16384")),
            "{message}"
        );
        assert_eq!(out.written, 0);
        frame
            .encode_image(&options(ImageFormat::WebpLossless))
            .unwrap();
        frame
            .encode_image(&options(ImageFormat::WebpLossy(75)).downscale(Downscale::Half))
            .unwrap();
    }
    let frame = VideoFrame::builder().resolution(32_766, 2).build().unwrap();
    let image = frame
        .encode_image(&options(ImageFormat::WebpLossy(75)).downscale(Downscale::Half))
        .unwrap();
    assert_eq!(decode_webp(&image).width, 16_383);
}

#[test]
fn a_failing_writer_is_reported_as_its_own_io_error() {
    let frame = random_codes(PixelFormat::Uyvy, 256, 64, 1).frame(None);
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg(85),
        ImageFormat::WebpLossless,
        ImageFormat::WebpLossy(75),
    ] {
        // In the header, and halfway through the image data.
        let len = frame.encode_image(&options(format)).unwrap().len();
        for fail_after in [0, 40, len / 2] {
            let mut out = Recorder {
                written: 0,
                fail_after: Some(fail_after),
            };
            match frame.encode_image_to(&options(format), &mut out) {
                Err(Error::Io(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                    assert!(
                        error.get_ref().is_some_and(|inner| inner.is::<DiskFull>()),
                        "{format:?} after {fail_after}: not the writer's own error: {error:?}"
                    );
                }
                other => panic!("{format:?} after {fail_after}: {other:?}"),
            }
        }
    }
}

#[test]
fn reduced_images_are_the_block_averages_of_the_full_image() {
    for format in [
        PixelFormat::Uyva,
        PixelFormat::Nv12,
        PixelFormat::Bgra,
        PixelFormat::Nb4g,
    ] {
        let (width, height) = (22usize, 14usize);
        let frame = VideoFrame::builder()
            .resolution(width as u32, height as u32)
            .pixel_format(format)
            .data(any_frame_bytes(format, width, height, 77))
            .build()
            .unwrap();
        let full = decode_png(&frame.encode_png().unwrap());
        for (downscale, size) in [
            (Downscale::Half, 2),
            (Downscale::Quarter, 4),
            (Downscale::Eighth, 8),
        ] {
            let reduced = decode_png(
                &frame
                    .encode_image(&options(ImageFormat::Png).downscale(downscale))
                    .unwrap(),
            );
            let (rw, rh) = (width.div_ceil(size), height.div_ceil(size));
            assert_eq!((reduced.width as usize, reduced.height as usize), (rw, rh));
            for by in 0..rh {
                for bx in 0..rw {
                    let pixels: Vec<&[u16]> = (by * size..((by + 1) * size).min(height))
                        .flat_map(|y| {
                            (bx * size..((bx + 1) * size).min(width)).map(move |x| (x, y))
                        })
                        .map(|(x, y)| &full.samples[(y * width + x) * 4..][..4])
                        .collect();
                    let count = pixels.len() as u64;
                    let alpha: u64 = pixels.iter().map(|p| u64::from(p[3])).sum();
                    let has_alpha = matches!(
                        format,
                        PixelFormat::Uyva | PixelFormat::Bgra | PixelFormat::Nb4g
                    );
                    let expected: Vec<u16> = (0..4)
                        .map(|c| {
                            let mean = |sum: u64, n: u64| ((sum + n / 2) / n) as u16;
                            if c == 3 {
                                mean(alpha, count)
                            } else if has_alpha && alpha > 0 {
                                mean(
                                    pixels
                                        .iter()
                                        .map(|p| u64::from(p[c]) * u64::from(p[3]))
                                        .sum(),
                                    alpha,
                                )
                            } else {
                                mean(pixels.iter().map(|p| u64::from(p[c])).sum(), count)
                            }
                        })
                        .collect();
                    assert_eq!(
                        &reduced.samples[(by * rw + bx) * 4..][..4],
                        expected,
                        "{format:?} {downscale:?} {bx},{by}"
                    );
                }
            }
            let jpeg = frame
                .encode_image(&options(ImageFormat::Jpeg(85)).downscale(downscale))
                .unwrap();
            let webp = decode_webp(
                &frame
                    .encode_image(&options(ImageFormat::WebpLossless).downscale(downscale))
                    .unwrap(),
            );
            assert_eq!((webp.width as usize, webp.height as usize), (rw, rh));
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        }
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Conversion allocates at most one output row, never in proportion to the
/// frame's height, apart from WebP's one buffer of exactly the image.
#[test]
fn conversion_scratch_is_bounded_and_webp_holds_exactly_the_image() {
    use crate::test_alloc::{allocation_count, peak_bytes};
    let width = 18usize;
    for format in PixelFormat::ALL {
        for height in [2usize, 18, 258] {
            let frame = VideoFrame::builder()
                .resolution(width as u32, height as u32)
                .pixel_format(format)
                .data(any_frame_bytes(format, width, height, 8))
                .build()
                .unwrap();
            let source = || Source::new(&frame);
            for downscale in [
                Downscale::Off,
                Downscale::Half,
                Downscale::Quarter,
                Downscale::Eighth,
            ] {
                let shift = downscale.shift();
                let reduced = (width.div_ceil(1 << shift), height.div_ceil(1 << shift));
                for (alpha, channels) in [(AlphaMode::Keep, 4), (AlphaMode::Discard, 3)] {
                    // PNG at both depths: the pipeline allocates only the
                    // downscale sums, then one row of scratch serves every row.
                    for depth in [SampleDepth::Eight, SampleDepth::Sixteen] {
                        let options = options(ImageFormat::Png)
                            .alpha(alpha)
                            .depth(depth)
                            .downscale(downscale);
                        let plan = Plan::new(&source(), &options).unwrap();
                        let source = source();
                        let bytes_per_sample = if depth == SampleDepth::Eight { 1 } else { 2 };
                        let sums = if shift == 0 {
                            0
                        } else {
                            reduced.0 * 16 * bytes_per_sample
                        };
                        let check = |allocations: usize, peak: usize| {
                            assert_eq!(
                                allocations,
                                usize::from(shift > 0),
                                "{format:?} {height} {options:?}"
                            );
                            assert_eq!(peak, sums, "{format:?} {height} {options:?}");
                        };
                        let row_len = reduced.0 * channels * bytes_per_sample;
                        if depth == SampleDepth::Eight {
                            let ((mut pixels, allocations), peak) =
                                peak_bytes(|| allocation_count(|| plan.pixels::<u8>(&source)));
                            check(allocations, peak);
                            let borrowed = pixels.rgba_rows().is_some();
                            assert_eq!(
                                borrowed,
                                format == PixelFormat::Rgba
                                    && alpha == AlphaMode::Keep
                                    && shift == 0
                            );
                            let (mut row, allocations) =
                                allocation_count(|| row_buffer::<u8>(reduced.0, channels));
                            assert_eq!(
                                (allocations, row.len(), row.capacity()),
                                (1, row_len, row_len)
                            );
                            let (_, allocations) = allocation_count(|| {
                                (0..reduced.1).for_each(|y| pixels.pack_row(y, channels, &mut row))
                            });
                            assert_eq!(allocations, 0, "{format:?} {height} {options:?}");
                        } else {
                            let ((mut pixels, allocations), peak) =
                                peak_bytes(|| allocation_count(|| plan.pixels::<u16>(&source)));
                            check(allocations, peak);
                            assert!(pixels.rgba_rows().is_none());
                            let mut row = row_buffer::<u16>(reduced.0, channels);
                            assert_eq!(row.len(), row_len);
                            let (_, allocations) = allocation_count(|| {
                                (0..reduced.1).for_each(|y| pixels.pack_row(y, channels, &mut row))
                            });
                            assert_eq!(allocations, 0, "{format:?} {height} {options:?}");
                        }
                    }
                    // WebP: one buffer of exactly the image, unless the
                    // frame's RGBA is read in place.
                    for (format, channels) in [
                        (ImageFormat::WebpLossless, channels),
                        (ImageFormat::WebpLossy(75), 4),
                    ] {
                        let options = options(format).alpha(alpha).downscale(downscale);
                        let plan = Plan::new(&source(), &options).unwrap();
                        let source = source();
                        let mut pixels = plan.pixels::<u8>(&source);
                        let ((image, allocations), peak) =
                            peak_bytes(|| allocation_count(|| webp::image(&mut pixels, channels)));
                        let len = reduced.0 * reduced.1 * channels;
                        let borrowed = matches!(image, std::borrow::Cow::Borrowed(_));
                        let expected_allocation = if borrowed { 0 } else { len };
                        assert_eq!(
                            (allocations, peak, image.len()),
                            (usize::from(!borrowed), expected_allocation, len),
                            "{format:?} {height} {options:?}"
                        );
                        assert!(
                            !borrowed
                                || source.format == PixelFormat::Rgba
                                    && alpha == AlphaMode::Keep
                                    && shift == 0
                        );
                    }
                }
                // JPEG: pixels convert straight into the codec's component
                // rows, so with those reserved nothing is allocated.
                let options = options(ImageFormat::Jpeg(85)).downscale(downscale);
                let plan = Plan::new(&source(), &options).unwrap();
                let source = source();
                let mut pixels = plan.pixels::<u8>(&source);
                let (width, height) = (pixels.width() as u16, pixels.height() as u16);
                let rows = jpeg::Rows {
                    pixels: std::cell::RefCell::new(&mut pixels),
                    width,
                    height,
                };
                let mut buffers = std::array::from_fn(|_| Vec::with_capacity(reduced.0));
                let (_, allocations) = allocation_count(|| {
                    for y in (0..height).chain([height - 1]) {
                        buffers.iter_mut().for_each(Vec::clear);
                        jpeg_encoder::ImageBuffer::fill_buffers(&rows, y, &mut buffers);
                        assert_eq!(buffers[0].len(), reduced.0);
                    }
                });
                assert_eq!(allocations, 0, "{format:?} {height} {options:?}");
            }
        }
    }
}

/// Manual measurement of encoding time and peak memory, whose results
/// `docs/image-encoding-performance.md` records:
///
/// `cargo test --release --lib --features image-encoding image_encoding_resources -- --ignored --nocapture`
///
/// `GRAFTON_NDI_PHOTO` and `GRAFTON_NDI_GRAPHICS` may each name a raw RGBA
/// file of 1920×1080 pixels, scaled to each size measured: a photograph, and
/// graphics. Without them synthetic stand-ins are used, which compress
/// differently. `GRAFTON_NDI_SIZES` may limit the sizes, as
/// `1920x1080,3840x2160`.
/// `GRAFTON_NDI_OUTPUTS` may select comma-separated output names, for example
/// `jpeg85,webp-lossy75,webp-lossy75-effort0,webp-lossy75-fast`.
#[test]
#[ignore = "Manual image encoding time and peak-allocation measurement"]
fn image_encoding_resources() {
    use crate::test_alloc::peak_bytes;
    use std::{hint::black_box, time::Instant};

    let input = |name: &str| std::env::var_os(name).map(|path| std::fs::read(path).unwrap());
    let (photo, graphics) = (input("GRAFTON_NDI_PHOTO"), input("GRAFTON_NDI_GRAPHICS"));
    let selected = std::env::var("GRAFTON_NDI_OUTPUTS").ok();
    let dump = std::env::var_os("GRAFTON_NDI_IMAGE_DUMP").map(std::path::PathBuf::from);
    if let Some(path) = &dump {
        std::fs::create_dir_all(path).unwrap();
    }
    let sizes: Vec<(usize, usize)> = std::env::var("GRAFTON_NDI_SIZES")
        .unwrap_or_else(|_| "1920x1080,3840x2160,7680x4320".into())
        .split(',')
        .map(|size| {
            let (width, height) = size.split_once('x').unwrap();
            (width.parse().unwrap(), height.parse().unwrap())
        })
        .collect();
    eprintln!("source,content,width,height,output,median_ms,peak_encode_bytes,output_bytes");
    for (width, height) in sizes {
        for (content, input) in [("photographic", &photo), ("graphics", &graphics)] {
            let rgba = content_rgba(content, width, height, input.as_deref());
            eprintln!(
                "input,{content},{width},{height},sha256={:02x?}",
                sha256(&rgba)
            );
            if let Some(path) = &dump {
                std::fs::write(path.join(format!("{content}-{width}x{height}.rgba")), &rgba)
                    .unwrap();
            }
            for format in [
                PixelFormat::Rgba,
                PixelFormat::Uyvy,
                PixelFormat::Nv12,
                PixelFormat::P216,
                PixelFormat::Na4g,
            ] {
                let frame = frame_from_rgba(format, width, height, &rgba);
                let outputs = [
                    (
                        "png-fast",
                        options(ImageFormat::Png).png_compression(PngCompression::Fast),
                    ),
                    ("png-balanced", options(ImageFormat::Png)),
                    (
                        "png-high",
                        options(ImageFormat::Png).png_compression(PngCompression::High),
                    ),
                    (
                        "png16-fast",
                        options(ImageFormat::Png)
                            .depth(SampleDepth::Sixteen)
                            .png_compression(PngCompression::Fast),
                    ),
                    (
                        "png16-balanced",
                        options(ImageFormat::Png).depth(SampleDepth::Sixteen),
                    ),
                    ("jpeg85", options(ImageFormat::Jpeg(85))),
                    ("webp", options(ImageFormat::WebpLossless)),
                    ("webp-lossy75", options(ImageFormat::WebpLossy(75))),
                    (
                        "webp-lossy75-effort0",
                        options(ImageFormat::WebpLossy(75)).webp_effort(0),
                    ),
                    (
                        "webp-lossy75-fast",
                        options(ImageFormat::WebpLossy(75))
                            .webp_effort(0)
                            .webp_sharp_yuv(false),
                    ),
                    (
                        "png-fast-quarter",
                        options(ImageFormat::Png)
                            .png_compression(PngCompression::Fast)
                            .downscale(Downscale::Quarter),
                    ),
                    (
                        "jpeg85-quarter",
                        options(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter),
                    ),
                    (
                        "webp-quarter",
                        options(ImageFormat::WebpLossless).downscale(Downscale::Quarter),
                    ),
                    (
                        "webp-lossy75-quarter",
                        options(ImageFormat::WebpLossy(75)).downscale(Downscale::Quarter),
                    ),
                ];
                for (name, options) in outputs {
                    if selected
                        .as_ref()
                        .is_some_and(|selected| !selected.split(',').any(|output| output == name))
                    {
                        continue;
                    }
                    let slow = name.contains("balanced") || name.contains("high");
                    let runs = if slow || width > 3840 { 1 } else { 5 };
                    let mut times = Vec::new();
                    let (mut peak, mut len) = (0, 0);
                    black_box(frame.encode_image(&options).unwrap());
                    for sample in 0..runs {
                        let start = Instant::now();
                        let (image, bytes) =
                            peak_bytes(|| frame.encode_image(black_box(&options)).unwrap());
                        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                        times.push(elapsed);
                        peak = peak.max(bytes);
                        len = image.len();
                        eprintln!("sample,{format:?},{content},{width},{height},{name},{sample},{elapsed:.6},{bytes},{len}");
                        if sample == 0 {
                            if let Some(path) = &dump {
                                std::fs::write(
                                    path.join(format!(
                                        "{format:?}-{content}-{width}x{height}-{name}.image"
                                    )),
                                    &image,
                                )
                                .unwrap();
                            }
                        }
                        black_box(image);
                    }
                    times.sort_by(f64::total_cmp);
                    eprintln!(
                        "{format:?},{content},{width},{height},{name},{:.1},{peak},{len}",
                        times[times.len() / 2]
                    );
                }
            }
        }
    }
}

/// RGBA content for the measurement: a 1920×1080 input scaled to fit, or a
/// synthetic stand-in.
fn content_rgba(content: &str, width: usize, height: usize, input: Option<&[u8]>) -> Vec<u8> {
    let mut random = XorShift(0xBEEF);
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let pixel: [u8; 4] = match (content, input) {
                (_, Some(input)) => {
                    let (sx, sy) = (x * 1920 / width, y * 1080 / height);
                    input[(sy * 1920 + sx) * 4..][..4].try_into().unwrap()
                }
                ("photographic", None) => {
                    let noise = (random.next() % 24) as usize;
                    [
                        ((x * 255 / width + noise) % 256) as u8,
                        ((y * 255 / height + noise / 2) % 256) as u8,
                        (((x + y) * 128 / (width + height) + noise) % 256) as u8,
                        255,
                    ]
                }
                _ => {
                    let in_box = (x / 240 + y / 135) % 3 == 0;
                    let text = (x % 97 < 3 || y % 61 < 2) && (x / 500) % 2 == 0;
                    match (in_box, text) {
                        (_, true) => [255, 255, 255, 255],
                        (true, _) => [20, 60, 140, 255],
                        _ => [(x * 255 / width) as u8, 32, 64, 255],
                    }
                }
            };
            out.extend_from_slice(&pixel);
        }
    }
    out
}

/// A frame of `format` showing `rgba`, converted with BT.709 limited range.
fn frame_from_rgba(format: PixelFormat, width: usize, height: usize, rgba: &[u8]) -> VideoFrame {
    if format == PixelFormat::Rgba {
        return VideoFrame::builder()
            .resolution(width as u32, height as u32)
            .pixel_format(format)
            .data(rgba.to_vec())
            .build()
            .unwrap();
    }
    let bits = sampling(format).bits;
    let s = sampling(format);
    let pixel = |x: usize, y: usize| {
        let p = &rgba[(y * width + x) * 4..][..3];
        forward(
            YuvMatrix::Bt709,
            YuvRange::Limited,
            bits,
            [p[0], p[1], p[2]],
        )
    };
    let cw = width >> s.h_shift;
    Codes::new(format, width, height, |site| match site {
        Site::Luma(i) => pixel(i % width, i / width)[0],
        Site::Cb(i) => pixel((i % cw) << s.h_shift, (i / cw) << s.v_shift)[1],
        Site::Cr(i) => pixel((i % cw) << s.h_shift, (i / cw) << s.v_shift)[2],
        Site::Alpha(_) => ((1u32 << bits) - 1) as u16,
    })
    .frame(None)
}
