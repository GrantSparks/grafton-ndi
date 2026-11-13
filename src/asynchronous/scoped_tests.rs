//! Payload accounting excludes task scheduling and native allocations: it
//! measures the shared processing scope over synthetic validated guards.
use super::process_captured;
use crate::{
    capture::{AudioKind, CaptureStrategy, Guard, MetadataKind, VideoKind},
    sys::bindings::*,
    test_alloc::{allocation_count, peak_bytes},
    ReceiverItemRef, VideoFrameRef,
};
use std::{cell::Cell, mem::MaybeUninit, ptr};

thread_local! { static FREES: Cell<usize> = const { Cell::new(0) }; }

fn video<'a>(
    pixels: &'a mut [MaybeUninit<u8>],
    width: i32,
    height: i32,
    stride: i32,
) -> VideoFrameRef<'a> {
    video_with_format(
        pixels,
        width,
        height,
        stride,
        NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
    )
}

fn video_with_format<'a>(
    pixels: &'a mut [MaybeUninit<u8>],
    width: i32,
    height: i32,
    stride: i32,
    format: NDIlib_FourCC_video_type_e,
) -> VideoFrameRef<'a> {
    let raw = NDIlib_video_frame_v2_t {
        xres: width,
        yres: height,
        FourCC: format,
        frame_rate_N: 30_000,
        frame_rate_D: 1001,
        p_data: pixels.as_mut_ptr().cast(),
        __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
            line_stride_in_bytes: stride,
        },
        p_metadata: c"<video/>".as_ptr(),
        ..Default::default()
    };
    // SAFETY: pixels contains every active row, remains borrowed for 'a,
    // and the synthetic free counts releases without touching that storage.
    unsafe {
        VideoKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {
            FREES.set(FREES.get() + 1);
        }))
        .unwrap()
    }
}

#[test]
fn seeded_padded_rows_match_owned_oracle_without_allocations() {
    let mut seed = 137_u32;
    for _ in 0..100 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let width = 2 * (1 + (seed % 16) as i32);
        let height = 2 * (1 + ((seed >> 5) % 8) as i32);
        let padding = 4 * (1 + ((seed >> 9) % 5) as i32);
        let (format, stride, planes) = match seed % 3 {
            0 => {
                let stride = width * 4 + padding;
                (
                    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
                    stride,
                    vec![(0, stride, width * 4, height)],
                )
            }
            1 => {
                let stride = width + padding;
                let chroma = stride / 2 * (height / 2);
                (
                    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420,
                    stride,
                    vec![
                        (0, stride, width, height),
                        (stride * height, stride / 2, width / 2, height / 2),
                        (stride * height + chroma, stride / 2, width / 2, height / 2),
                    ],
                )
            }
            _ => {
                let stride = width * 2 + padding;
                (
                    NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA,
                    stride,
                    vec![
                        (0, stride, width * 2, height),
                        (stride * height, stride / 2, width, height),
                    ],
                )
            }
        };
        let span = planes
            .iter()
            .map(|&(offset, stride, _, rows)| offset + stride * rows)
            .max()
            .unwrap();
        let mut pixels = vec![MaybeUninit::uninit(); span as usize];
        for &(offset, stride, active, rows) in &planes {
            for row in 0..rows {
                for column in 0..active {
                    pixels[(offset + row * stride + column) as usize].write((row + column) as u8);
                }
            }
        }
        let frame = video_with_format(&mut pixels, width, height, stride, format);
        let oracle = frame.to_owned();
        let before = FREES.get();
        let ((sum, meta), count) = allocation_count(|| {
            process_captured(Some(ReceiverItemRef::Video(frame)), |item| {
                let ReceiverItemRef::Video(frame) = item else {
                    unreachable!()
                };
                let sum = frame
                    .planes()
                    .flat_map(|p| p.rows())
                    .flatten()
                    .map(|&b| u64::from(b))
                    .sum::<u64>();
                (sum, frame.metadata().unwrap().len())
            })
            .unwrap()
        });
        assert_eq!(count, 0, "seed {seed}: padding must not force a copy");
        assert_eq!(
            sum,
            oracle.data().iter().map(|&b| u64::from(b)).sum::<u64>()
        );
        assert_eq!(meta, oracle.metadata().unwrap().len());
        assert_eq!(FREES.get(), before + 1);
    }
}

#[test]
fn strided_channels_and_text_have_no_processing_allocations() {
    let mut samples = [MaybeUninit::<f32>::uninit(); 16];
    for (i, value) in [(0, 0.25), (1, 0.5), (4, -0.25), (5, -0.5)] {
        samples[i].write(value);
    }
    let raw = NDIlib_audio_frame_v3_t {
        sample_rate: 48_000,
        no_channels: 2,
        no_samples: 2,
        FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
        p_data: samples.as_mut_ptr().cast(),
        __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
            channel_stride_in_bytes: 16,
        },
        p_metadata: c"<audio/>".as_ptr(),
        ..Default::default()
    };
    // SAFETY: the initialized channel samples and static metadata outlive the
    // guard. The synthetic free never touches independently owned storage.
    let frame = unsafe {
        AudioKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {
            FREES.set(FREES.get() + 1);
        }))
        .unwrap()
    };
    let oracle = frame.to_owned();
    let native = samples.as_ptr() as usize;
    let before = FREES.get();
    let (summary, allocations) = allocation_count(|| {
        process_captured(Some(frame), |frame| {
            assert!(frame.data().is_none());
            assert_eq!(frame.channel_data(0).unwrap().as_ptr() as usize, native);
            (
                frame.channel_data(0).unwrap().iter().sum::<f32>(),
                frame.channel_data(1).unwrap().iter().sum::<f32>(),
                frame.metadata().unwrap().len(),
            )
        })
        .unwrap()
    });
    assert_eq!(allocations, 0);
    assert_eq!(
        summary,
        (
            oracle.data()[..2].iter().sum::<f32>(),
            oracle.data()[2..].iter().sum::<f32>(),
            8
        )
    );
    assert_eq!(FREES.get(), before + 1);

    let text = c"<metadata/>";
    let raw = NDIlib_metadata_frame_t {
        length: 0,
        timecode: 3456,
        p_data: text.as_ptr().cast_mut(),
    };
    // SAFETY: static NUL-terminated text outlives the guard and is never freed
    // by the counting callback.
    let frame = unsafe {
        MetadataKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {
            FREES.set(FREES.get() + 1);
        }))
        .unwrap()
    };
    let (length, allocations) = allocation_count(|| {
        process_captured(Some(ReceiverItemRef::Metadata(frame)), |item| {
            let ReceiverItemRef::Metadata(frame) = item else {
                unreachable!()
            };
            assert_eq!(frame.data().as_ptr(), text.as_ptr().cast());
            frame.data().len()
        })
        .unwrap()
    });
    assert_eq!((length, allocations), (11, 0));
    assert_eq!(FREES.get(), before + 2);
}

#[test]
fn processing_panic_releases_guard() {
    let mut pixels = vec![MaybeUninit::new(37); 32];
    let frame = video(&mut pixels, 4, 2, 16);
    let before = FREES.get();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        process_captured(Some(frame), |_| panic!("processing panic"));
    }));
    assert!(result.is_err());
    assert_eq!(FREES.get(), before + 1);
}

/// Ownership conversion only; excludes SDK encoding, capture and scheduling.
/// Run in release mode with --ignored --nocapture to record median/p99 times.
#[test]
#[ignore = "manual payload-copy benchmark"]
fn payload_copy_benchmark() {
    use std::{hint::black_box, time::Instant};
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        for padding in [0, 64] {
            let stride = width * 4 + padding;
            let mut pixels = vec![MaybeUninit::uninit(); (stride * height) as usize];
            for row in 0..height as usize {
                pixels[row * stride as usize..row * stride as usize + width as usize * 4]
                    .fill(MaybeUninit::new(37));
            }
            let frame = video(&mut pixels, width, height, stride);
            let inspect = |frame: &VideoFrameRef<'_>| {
                black_box((
                    frame.width(),
                    frame.height(),
                    frame.planes().next().unwrap().rows().next().unwrap()[0],
                ))
            };
            let (_, scoped_allocs) =
                allocation_count(|| process_captured(Some(&frame), |f| inspect(f)));
            let (_, scoped_bytes) = peak_bytes(|| process_captured(Some(&frame), |f| inspect(f)));
            let (_, owned_bytes) = peak_bytes(|| {
                process_captured(Some(&frame), |f| {
                    let owned = (*f).to_owned();
                    black_box((owned.width(), owned.height(), owned.data()[0]))
                })
            });
            assert_eq!((scoped_allocs, scoped_bytes), (0, 0));
            let mut scoped = Vec::new();
            let mut owned = Vec::new();
            for i in 0..510 {
                let began = Instant::now();
                process_captured(Some(&frame), |f| inspect(f));
                let scoped_time = began.elapsed();
                let began = Instant::now();
                process_captured(Some(&frame), |f| {
                    let copy = (*f).to_owned();
                    black_box((copy.width(), copy.height(), copy.data()[0]));
                    black_box(&copy);
                });
                let owned_time = began.elapsed();
                if i >= 10 {
                    scoped.push(scoped_time);
                    owned.push(owned_time);
                }
            }
            scoped.sort_unstable();
            owned.sort_unstable();
            eprintln!("{width}x{height} padding={padding}: scoped median={:?} p99={:?} bytes={scoped_bytes}; owned median={:?} p99={:?} bytes={owned_bytes}",
                scoped[250], scoped[494], owned[250], owned[494]);
        }
    }
}

#[test]
fn seeded_strided_channels_match_owned_oracle_without_allocations() {
    let mut seed = 137_u32;
    for _ in 0..100 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let channels = 1 + seed as usize % 8;
        let samples = 1 + (seed >> 4) as usize % 64;
        let stride = samples + 1 + (seed >> 10) as usize % 16;
        let mut data = vec![MaybeUninit::<f32>::uninit(); channels * stride];
        for channel in 0..channels {
            for sample in 0..samples {
                data[channel * stride + sample].write((channel + sample) as f32 / 64.0);
            }
        }
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: channels as i32,
            no_samples: samples as i32,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: (stride * 4) as i32,
            },
            p_metadata: c"<audio/>".as_ptr(),
            ..Default::default()
        };
        // SAFETY: only active samples are initialized; the descriptor's strides
        // describe that live storage. Counting frees never touch the allocation.
        let frame = unsafe {
            AudioKind::make_ref(Guard::with_test_free(ptr::null_mut(), raw, |_, _| {
                FREES.set(FREES.get() + 1);
            }))
            .unwrap()
        };
        let oracle = frame.to_owned();
        let before = FREES.get();
        let (sum, allocations) = allocation_count(|| {
            process_captured(Some(ReceiverItemRef::Audio(frame)), |item| {
                let ReceiverItemRef::Audio(frame) = item else {
                    unreachable!()
                };
                (0..channels)
                    .flat_map(|ch| frame.channel_data(ch).unwrap())
                    .sum::<f32>()
            })
            .unwrap()
        });
        assert_eq!(sum, oracle.data().iter().sum::<f32>());
        assert_eq!(
            allocations, 0,
            "seed {seed}: channel padding must not force a copy"
        );
        assert_eq!(FREES.get(), before + 1);
    }
}
