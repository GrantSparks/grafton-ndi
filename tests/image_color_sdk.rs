//! The NDI SDK's own color handling, which image encoding's default color
//! policy follows, pinned against a real runtime on loopback.
//!
//! Measured with the 6.4 SDK: the SDK converts between YUV and RGB with
//! BT.709 limited range at every frame size, ignores `ndi_color_info` when it
//! converts but passes it through, and repeats chroma, both when it upsamples
//! to RGB and when it turns 4:2:0 into 4:2:2. The encoder follows that with
//! exact coefficients, so its PNG of a UYVY frame stays within 10 code values
//! of the SDK's RGBX frame of the same source, and within 1.5 on average. A
//! later SDK that changes any of this fails here, instead of silently
//! changing snapshots.
//!
//! Run with `--config ci/offline.toml`, which keeps the SDK on loopback.
//! Every conversion is judged against the UYVY frame a second receiver got
//! from the same stream, so the tests do not depend on the transport being
//! lossless. It is for these frames on Linux x86-64, which the first test
//! pins; the 32-bit x86 and the Windows runtimes were seen not to return a
//! 3840x2160 frame exactly.
#![cfg(feature = "image-encoding")]

mod common;

use grafton_ndi::{
    ColorInfo, ColorPolicy, Error, FrameSync, ImageFormat, ImageOptions, Ndi, PixelFormat,
    Receiver, ReceiverColorFormat, ReceiverOptions, ScanType, Sender, SenderOptions, VideoFrame,
    YuvMatrix, YuvRange,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// The YCbCr patches: black, white, mid-grey, sub-black, super-white and the
/// chroma extremes, then a grid of luma and chroma.
fn patches() -> Vec<[u8; 3]> {
    let mut patches = vec![
        [16, 128, 128],
        [235, 128, 128],
        [126, 128, 128],
        [4, 128, 128],
        [250, 128, 128],
        [128, 16, 128],
        [128, 240, 128],
        [128, 128, 16],
        [128, 128, 240],
    ];
    for y in [64, 128, 192] {
        for cb in [48, 128, 208] {
            for cr in [48, 128, 208] {
                patches.push([y, cb, cr]);
            }
        }
    }
    patches
}

/// A UYVY frame of 64×64 patches.
fn uyvy_patches(width: usize, height: usize, metadata: &str) -> VideoFrame {
    let patches = patches();
    let columns = width.div_ceil(64);
    let at = |x: usize, y: usize| patches[((y / 64) * columns + x / 64) % patches.len()];
    let mut data = Vec::with_capacity(width * height * 2);
    for y in 0..height {
        for k in 0..width / 2 {
            let [y0, cb, cr] = at(2 * k, y);
            let y1 = at(2 * k + 1, y)[0];
            data.extend_from_slice(&[cb, y0, cr, y1]);
        }
    }
    frame(PixelFormat::Uyvy, width, height, data, metadata)
}

fn frame(
    format: PixelFormat,
    width: usize,
    height: usize,
    data: Vec<u8>,
    metadata: &str,
) -> VideoFrame {
    VideoFrame::builder()
        .resolution(width as u32, height as u32)
        .pixel_format(format)
        .frame_rate(grafton_ndi::FrameRate::new(25, 1).unwrap())
        .data(data)
        .metadata(metadata)
        .build()
        .unwrap()
}

/// Stops a sending loop when the scope that waits for it ends, by return or
/// by panic: a scope joins its threads either way, so a loop that only a
/// later statement stops would hang a failed test instead of reporting it.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Send `frames` in rotation from one new sender, and receive from it in
/// each of `formats` until every receiver holds a copy of each frame, which
/// its metadata identifies. Returns one list a receiver, in `frames`' order.
fn round_trip(
    ndi: &Ndi,
    label: &str,
    frames: &[VideoFrame],
    formats: &[ReceiverColorFormat],
) -> Vec<Vec<VideoFrame>> {
    let sender = Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(label)).clock_video(false),
    )
    .unwrap();
    let source = common::loopback_source(ndi, &sender).unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for frame in frames.iter().cycle() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                sender.send_video(frame).unwrap();
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let _stop = StopOnDrop(&stop);
        let received: Vec<Vec<VideoFrame>> = formats
            .iter()
            .map(|&format| {
                let receiver = Receiver::new(
                    ndi,
                    &ReceiverOptions::new(source.clone()).color_format(format),
                )
                .unwrap();
                collect(
                    frames,
                    |timeout| receiver.video().capture(timeout).unwrap(),
                    &format!("{label} {format:?}"),
                )
            })
            .collect();
        received
    })
}

/// Capture until a frame like each of `frames`, by metadata and size, has
/// arrived.
fn collect(
    frames: &[VideoFrame],
    mut capture: impl FnMut(Duration) -> Option<VideoFrame>,
    what: &str,
) -> Vec<VideoFrame> {
    let mut found: Vec<Option<VideoFrame>> = vec![None; frames.len()];
    let deadline = Instant::now() + Duration::from_secs(60);
    while found.iter().any(Option::is_none) {
        assert!(Instant::now() < deadline, "{what}: not every frame arrived");
        let Some(frame) = capture(Duration::from_millis(500)) else {
            continue;
        };
        for (sent, slot) in frames.iter().zip(&mut found) {
            if (sent.width(), sent.height(), sent.metadata())
                == (frame.width(), frame.height(), frame.metadata())
            {
                *slot = Some(frame.clone());
            }
        }
    }
    found.into_iter().map(Option::unwrap).collect()
}

/// The RGB of a decoded 8-bit PNG.
fn png_rgb(png: &[u8]) -> Vec<[u8; 3]> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Rgba, png::BitDepth::Eight)
    );
    buffer
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[r, g, b, _]| [r, g, b])
        .collect()
}

/// The RGB of an RGBX or RGBA frame's active rows.
fn frame_rgb(frame: &VideoFrame) -> Vec<[u8; 3]> {
    assert!(matches!(
        frame.pixel_format(),
        PixelFormat::Rgbx | PixelFormat::Rgba
    ));
    frame
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[r, g, b, _]| [r, g, b])
        .collect()
}

/// The largest and the mean difference between two buffers of one length.
fn byte_difference(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let (mut largest, mut total) = (0, 0u64);
    for (a, b) in a.iter().zip(b) {
        let d = a.abs_diff(*b);
        largest = largest.max(d);
        total += u64::from(d);
    }
    (largest, total as f64 / a.len() as f64)
}

/// Where two UYVY pictures of 64x64 flat patches, of one size, differ, for
/// a failure message: how many bytes, the rectangle of pixels that holds
/// them, the largest difference, and the patches that differ most. The
/// patches lie on the codec's block boundaries, so its loss alone leaves
/// every patch close to flat and spreads evenly; a region that differs much
/// more than the rest is a frame that is wrong there.
fn where_they_differ(sent: &[u8], received: &[u8], width: usize) -> String {
    let row = width * 2;
    let height = sent.len() / row;
    let (columns, rows) = (width.div_ceil(64), height.div_ceil(64));
    // The sum and the largest of the differences in each patch.
    let mut patches = vec![(0u64, 0u8); columns * rows];
    let mut differing = 0;
    let (mut left, mut right, mut top, mut bottom) = (usize::MAX, 0, usize::MAX, 0);
    let mut largest = (0, 0);
    for (i, (s, r)) in sent.iter().zip(received).enumerate() {
        let d = s.abs_diff(*r);
        if d == 0 {
            continue;
        }
        differing += 1;
        let (x, y) = (i % row / 2, i / row);
        (left, right, top, bottom) = (left.min(x), right.max(x), top.min(y), bottom.max(y));
        let patch = &mut patches[(y / 64) * columns + x / 64];
        patch.0 += u64::from(d);
        patch.1 = patch.1.max(d);
        if d > largest.0 {
            largest = (d, i);
        }
    }
    if differing == 0 {
        return "they are identical".to_owned();
    }
    let mean = |index: usize, total: u64| {
        let (px, py) = (index % columns, index / columns);
        let bytes = 64.min(width - px * 64) * 2 * 64.min(height - py * 64);
        total as f64 / bytes as f64
    };
    let mut worst: Vec<_> = patches
        .iter()
        .enumerate()
        .filter(|(_, (total, _))| *total > 0)
        .map(|(index, &(total, max))| (index, mean(index, total), max))
        .collect();
    let affected = worst.len();
    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (d, i) = largest;
    let mut text = format!(
        "{differing} of {} bytes differ, in pixels {left}..={right} of rows {top}..={bottom}; \
         {affected} of {} patches differ; the largest difference, {d}, is at byte {} of row {} \
         (sent {}, received {}); the patches that differ most, by mean:",
        sent.len(),
        columns * rows,
        i % row,
        i / row,
        sent[i],
        received[i]
    );
    for (index, mean, max) in worst.iter().take(6) {
        let (px, py) = (index % columns, index / columns);
        text += &format!(" ({px}, {py}) {mean:.2} up to {max};");
    }
    text
}

/// How far two UYVY pictures of 64x64 flat patches, of one size, are apart
/// where codec loss leaves a flat patch flat: the largest difference, over
/// every patch and component, between the means of a patch's interior, the
/// pixels at least 8 from its edges, with the patch and component it is in.
/// A wrong frame, or the SDK's black placeholder, differs here by tens of
/// code values; loss at the patches' edges, which can be large, does not.
fn interior_difference(sent: &[u8], received: &[u8], width: usize) -> (f64, String) {
    const INSET: usize = 8;
    let row = width * 2;
    let height = sent.len() / row;
    let (columns, rows) = (width.div_ceil(64), height.div_ceil(64));
    let mut worst = (0.0, "every patch interior".to_owned());
    for (py, px) in (0..rows).flat_map(|py| (0..columns).map(move |px| (py, px))) {
        let (x0, y0) = (px * 64 + INSET, py * 64 + INSET);
        let (x1, y1) = (
            (px * 64 + 64).min(width) - INSET,
            (py * 64 + 64).min(height) - INSET,
        );
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        // Sums of Cb, Y and Cr, sent and received, over the pixel pairs
        // inside; a pair is Cb, Y, Cr, Y.
        let mut sums = [[0u64; 3]; 2];
        let mut counts = [0u64; 3];
        for y in y0..y1 {
            for pair in x0.div_ceil(2)..x1 / 2 {
                let at = y * row + pair * 4;
                for (side, bytes) in [sent, received].into_iter().enumerate() {
                    let [cb, y0, cr, y1] = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
                    sums[side][0] += u64::from(cb);
                    sums[side][1] += u64::from(y0) + u64::from(y1);
                    sums[side][2] += u64::from(cr);
                }
                counts[0] += 1;
                counts[1] += 2;
                counts[2] += 1;
            }
        }
        for (component, name) in ["Cb", "Y", "Cr"].into_iter().enumerate() {
            let count = counts[component] as f64;
            let gap = (sums[0][component] as f64 - sums[1][component] as f64).abs() / count;
            if gap > worst.0 {
                worst = (gap, format!("{name} inside patch ({px}, {py})"));
            }
        }
    }
    worst
}

/// The largest and the mean difference between two images.
fn difference(a: &[[u8; 3]], b: &[[u8; 3]]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let mut largest = 0;
    let mut total = 0u64;
    for (a, b) in a.iter().zip(b) {
        for (a, b) in a.iter().zip(b) {
            let d = a.abs_diff(*b);
            largest = largest.max(d);
            total += u64::from(d);
        }
    }
    (largest, total as f64 / (a.len() * 3) as f64)
}

#[test]
fn the_sdk_converts_with_bt709_at_every_size_whatever_the_tag_says() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let untagged = "<image-color case=\"untagged\"/>";
    let bt601 = "<ndi_color_info matrix=\"bt_601\" primaries=\"bt_601\" transfer=\"bt_601\"/>";
    let bt2020 = "<ndi_metadata_group><ndi_color_info matrix=\"bt_2020\" primaries=\"bt_2020\" transfer=\"bt_2020\"/></ndi_metadata_group>";
    for (width, height) in [(720, 576), (1920, 1080), (3840, 2160)] {
        let sent: Vec<VideoFrame> = [untagged, bt601, bt2020]
            .into_iter()
            .map(|metadata| uyvy_patches(width, height, metadata))
            .collect();
        let received = round_trip(
            &ndi,
            &format!("image color {width}x{height}"),
            &sent,
            &[ReceiverColorFormat::UyvyBgra, ReceiverColorFormat::RgbxRgba],
        );
        for ((sent, uyvy), rgbx) in sent.iter().zip(&received[0]).zip(&received[1]) {
            let case = format!("{width}x{height} {:?}", sent.metadata());
            // Both receivers decode one stream, so the UYVY frame is what the
            // SDK converted to RGBX, whether or not it is the frame sent. It
            // is the picture sent: each patch keeps its value away from its
            // edges. The edges themselves need not survive. The 32-bit x86
            // runtime, in run 43366775596, returned this picture at 3840x2160
            // with 4% of its bytes changed, in 1812 of its 2040 patches and by
            // up to 224, the most at a patch edge, where it had taken the
            // neighbouring patch's chroma.
            assert_eq!(uyvy.pixel_format(), PixelFormat::Uyvy, "{case}");
            let (largest, mean) = byte_difference(uyvy.data(), sent.data());
            let (interior, at) = interior_difference(sent.data(), uyvy.data(), width);
            assert!(
                interior <= 2.0,
                "{case}: the received UYVY is not the picture sent: the mean of {at} differs by \
                 {interior:.2}; the frame differs by up to {largest}, {mean:.3} on average; {}",
                where_they_differ(sent.data(), uyvy.data(), width)
            );
            if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                assert_eq!(
                    (largest, mean),
                    (0, 0.0),
                    "{case}: the loopback transport was lossless for this frame on Linux x86-64"
                );
            }
            // The tag reaches both receivers unchanged.
            assert_eq!(
                uyvy.color_info().unwrap(),
                sent.color_info().unwrap(),
                "{case}"
            );
            assert_eq!(
                rgbx.color_info().unwrap(),
                sent.color_info().unwrap(),
                "{case}"
            );
            assert_eq!(rgbx.pixel_format(), PixelFormat::Rgbx, "{case}");
            // The SDK used BT.709 limited range: the encoder, told to do the
            // same, stays within the measured gap between exact BT.709 and the
            // SDK's fixed-point approximation of it.
            let bt709 = ImageOptions::new(ImageFormat::Png).color(ColorPolicy::Override {
                matrix: YuvMatrix::Bt709,
                range: YuvRange::Limited,
            });
            let (largest, mean) = difference(
                &png_rgb(&uyvy.encode_image(&bt709).unwrap()),
                &frame_rgb(rgbx),
            );
            assert!(largest <= 10 && mean <= 1.5, "{case}: the SDK's RGB differs from BT.709 by up to {largest}, {mean:.2} on average");
            // The default policy follows a tag and otherwise takes BT.709.
            let default = uyvy.encode_png().unwrap();
            if sent
                .color_info()
                .unwrap()
                .and_then(|info| info.matrix())
                .is_none()
            {
                assert_eq!(default, uyvy.encode_image(&bt709).unwrap(), "{case}");
            } else {
                assert_ne!(default, uyvy.encode_image(&bt709).unwrap(), "{case}");
            }
        }
    }
}

/// Chroma that changes only between SpeedHQ's macroblocks, 16 pixels by 8
/// rows, reaches receivers exactly, so the edges between blocks show how the
/// SDK sites chroma: repeated chroma keeps them sharp, where interpolation
/// would blend the pixels beside them. A single-sample impulse would not do,
/// because the codec rings around it.
#[test]
fn the_sdk_repeats_chroma_into_rgb_and_from_4_2_0() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let (width, height) = (64, 32);
    let (a, b) = ([200u8, 60u8], [60u8, 200u8]);
    // UYVY with flat grey luma and a checkerboard of chroma blocks.
    let block = |x: usize, y: usize| {
        if (x / 16 + y / 8).is_multiple_of(2) {
            a
        } else {
            b
        }
    };
    let mut data = Vec::new();
    for y in 0..height {
        for k in 0..width / 2 {
            let [cb, cr] = block(2 * k, y);
            data.extend_from_slice(&[cb, 126, cr, 126]);
        }
    }
    let uyvy = frame(
        PixelFormat::Uyvy,
        width,
        height,
        data,
        "<image-color case=\"422\"/>",
    );
    // NV12 whose chroma changes after 8 chroma rows, 16 luma rows.
    let chroma_row = |j: usize| if j < 8 { a } else { b };
    let mut data = vec![126; width * height];
    for j in 0..height / 2 {
        for _ in 0..width / 2 {
            data.extend_from_slice(&chroma_row(j));
        }
    }
    let nv12 = frame(
        PixelFormat::Nv12,
        width,
        height,
        data,
        "<image-color case=\"420\"/>",
    );
    let received = round_trip(
        &ndi,
        "image color chroma",
        &[uyvy.clone(), nv12],
        &[ReceiverColorFormat::RgbxRgba, ReceiverColorFormat::UyvyBgra],
    );
    assert!(
        received[1][0].data() == uyvy.data(),
        "the loopback assumption failed: block-flat UYVY did not arrive exactly"
    );
    // 4:2:2 to RGB: every pixel takes its own block's color, in the SDK's
    // RGB and in the encoder's, which stay within the measured gap.
    let sdk = frame_rgb(&received[0][0]);
    let encoder = png_rgb(&uyvy.encode_png().unwrap());
    // The SDK's fixed-point conversion alternates by one code between even
    // and odd pixels; a blend would sit far between the two colors.
    for (rgb, tolerance, whose) in [(&sdk, 1, "the SDK's"), (&encoder, 0, "the encoder's")] {
        let blended: Vec<(usize, usize)> = (0..width * height)
            .map(|i| (i % width, i / width))
            .filter(|&(x, y)| {
                let own = rgb[(y / 8 * 8) * width + x / 16 * 16];
                rgb[y * width + x]
                    .iter()
                    .zip(own)
                    .any(|(a, b)| a.abs_diff(b) > tolerance)
            })
            .collect();
        assert!(
            blended.is_empty(),
            "{whose} RGB blends pixels across chroma blocks: {blended:?}"
        );
    }
    let (largest, mean) = difference(&encoder, &sdk);
    assert!(
        largest <= 10 && mean <= 1.5,
        "up to {largest}, {mean:.2} on average"
    );
    // 4:2:0 to 4:2:2: each chroma row arrives as two identical rows, so the
    // edge between rows 15 and 16 stays sharp.
    let received = &received[1][1];
    assert_eq!(received.pixel_format(), PixelFormat::Uyvy);
    for y in 0..height {
        let row = received.plane(0).unwrap().row(y).unwrap();
        let chroma: Vec<[u8; 2]> = row
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[cb, _, cr, _]| [cb, cr])
            .collect();
        assert_eq!(chroma, vec![chroma_row(y / 2); width / 2], "4:2:2 row {y}");
    }
}

#[test]
fn the_sdk_sends_rgb_as_bt709_limited_range() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let (width, height) = (128, 16);
    // Black on the left, white on the right.
    let data: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            if i % width < width / 2 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        })
        .collect();
    let rgbx = frame(
        PixelFormat::Rgbx,
        width,
        height,
        data,
        "<image-color case=\"rgb\"/>",
    );
    let received = round_trip(
        &ndi,
        "image color rgb",
        &[rgbx],
        &[ReceiverColorFormat::UyvyBgra],
    );
    let uyvy = &received[0][0];
    assert_eq!(uyvy.pixel_format(), PixelFormat::Uyvy);
    for y in 0..height {
        let row = uyvy.plane(0).unwrap().row(y).unwrap();
        // Away from the edge, where the SDK filters chroma.
        assert_eq!(row[2 * 8 + 1], 16, "black luma");
        assert_eq!(row[2 * (width - 8) + 1], 235, "white luma");
    }
}

#[test]
fn a_pq_tag_reaches_rgb_receivers_and_their_frames_are_refused() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let (width, height) = (64, 16);
    let tag = "<ndi_color_info transfer=\"bt_2100_pq\" matrix=\"bt_2100\" primaries=\"bt_2100\"/>";
    let mut data = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            data.extend_from_slice(&((x as u16) << 10).to_ne_bytes());
        }
    }
    for _ in 0..height {
        for _ in 0..width / 2 {
            data.extend_from_slice(&0x8000u16.to_ne_bytes());
            data.extend_from_slice(&0x8000u16.to_ne_bytes());
        }
    }
    let p216 = frame(PixelFormat::P216, width, height, data, tag);
    let received = round_trip(
        &ndi,
        "image color pq",
        std::slice::from_ref(&p216),
        &[ReceiverColorFormat::RgbxRgba],
    );
    let rgbx = &received[0][0];
    let info: ColorInfo = rgbx
        .color_info()
        .unwrap()
        .expect("the PQ tag reaches the RGB receiver");
    assert_eq!(Some(info), p216.color_info().unwrap());
    assert!(info.is_hdr());
    match rgbx.encode_png() {
        Err(Error::InvalidFrame(message)) => assert!(message.contains("PQ"), "{message}"),
        other => panic!("an RGB frame tagged PQ must be refused, got {other:?}"),
    }
}

#[test]
fn framesync_frames_encode_like_the_frames_sent() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let sent = uyvy_patches(320, 180, "<image-color case=\"framesync\"/>");
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("image color framesync")).clock_video(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                sender.send_video(&sent).unwrap();
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let _stop = StopOnDrop(&stop);
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::new(source).color_format(ReceiverColorFormat::UyvyBgra),
        )
        .unwrap();
        let framesync = FrameSync::new(receiver).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "FrameSync delivered no frame");
            if let Some(frame) = framesync.capture_video_ref(ScanType::Progressive).unwrap() {
                if (frame.width(), frame.metadata()) == (320, sent.metadata()) {
                    assert_eq!(frame.encode_png().unwrap(), sent.encode_png().unwrap());
                    assert_eq!(
                        frame.encode_jpeg(85).unwrap(),
                        sent.encode_jpeg(85).unwrap()
                    );
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
}
