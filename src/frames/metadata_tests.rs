//! Optional metadata never makes otherwise valid media inaccessible.
use super::*;
use crate::capture::{FrameSyncAudioFree, FrameSyncVideoFree};
use std::cell::Cell;

thread_local! {
    static FREES: Cell<usize> = const { Cell::new(0) };
}

unsafe fn count_free<I, F>(_: I, _: &mut F) {
    FREES.with(|count| count.set(count.get() + 1));
}

fn video<S>(metadata: *const c_char, status: FrameMetadataStatus)
where
    S: FrameFree<RawFrame = NDIlib_video_frame_v2_t>,
    for<'a> VideoRef<'a, S>: VideoSource,
    S::Instance: Default,
{
    FREES.with(|count| count.set(0));
    let mut pixels = [37; 32];
    let raw = NDIlib_video_frame_v2_t {
        xres: 4,
        yres: 2,
        FourCC: FourCc::from(PixelFormat::Bgra).raw(),
        frame_rate_N: 30,
        frame_rate_D: 1,
        p_data: pixels.as_mut_ptr(),
        p_metadata: metadata,
        __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
            line_stride_in_bytes: 16,
        },
        ..Default::default()
    };
    let (frame, allocations) = crate::test_alloc::allocation_count(|| {
        // SAFETY: pixels and metadata stay live until this synthetic guard is
        // released. The callback counts release without freeing borrowed data.
        unsafe {
            VideoRef::<S>::new(Guard::with_test_free(
                S::Instance::default(),
                raw,
                count_free,
            ))
            .unwrap()
        }
    });
    assert_eq!(allocations, 0);
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        assert_eq!(VideoParts::metadata_state(&frame), status);
        assert_eq!(VideoSource::metadata_status(&frame), status);
        assert_eq!(VideoSource::data(&frame), Some(pixels.as_slice()));
        assert_eq!(
            frame.video_descriptor().p_metadata.is_null(),
            status != FrameMetadataStatus::Text
        );
    });
    assert_eq!(allocations, 0);
    let mut owned = frame.owned_copy();
    let owned_status = if status == FrameMetadataStatus::Text {
        FrameMetadataStatus::Text
    } else {
        FrameMetadataStatus::Absent
    };
    assert_eq!(owned.metadata_status(), owned_status);
    assert_eq!(owned.data(), pixels);
    frame.copy_into_owned(&mut owned);
    assert_eq!(owned.metadata_status(), owned_status);
    assert_eq!(
        owned.to_raw().p_metadata.is_null(),
        status != FrameMetadataStatus::Text
    );
    assert_eq!(FREES.with(Cell::get), 0);
    drop(frame);
    assert_eq!(FREES.with(Cell::get), 1);

    let invalid = NDIlib_video_frame_v2_t {
        p_data: ptr::null_mut(),
        ..raw
    };
    // SAFETY: invalid media is rejected before reading pixels; metadata stays live.
    let result = unsafe {
        VideoRef::<S>::new(Guard::with_test_free(
            S::Instance::default(),
            invalid,
            count_free,
        ))
    };
    assert!(matches!(result, Err(Error::InvalidFrame(_))));
    assert_eq!(FREES.with(Cell::get), 2);
}

fn audio<S>(metadata: *const c_char, status: FrameMetadataStatus)
where
    S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>,
    for<'a> AudioRef<'a, S>: AudioSource,
    S::Instance: Default,
{
    FREES.with(|count| count.set(0));
    let mut samples = [0.25f32; 4];
    let raw = NDIlib_audio_frame_v3_t {
        sample_rate: 48_000,
        no_channels: 2,
        no_samples: 2,
        FourCC: AudioFormat::Fltp.to_sdk(),
        p_data: samples.as_mut_ptr().cast(),
        p_metadata: metadata,
        __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
            channel_stride_in_bytes: 8,
        },
        ..Default::default()
    };
    let (frame, allocations) = crate::test_alloc::allocation_count(|| {
        // SAFETY: samples and metadata remain live until release; the callback
        // counts release without freeing these borrowed allocations.
        unsafe {
            AudioRef::<S>::from_guard(Guard::with_test_free(
                S::Instance::default(),
                raw,
                count_free,
            ))
            .unwrap()
        }
    });
    assert_eq!(allocations, 0);
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        assert_eq!(AudioParts::metadata_state(&frame), status);
        assert_eq!(AudioSource::metadata_status(&frame), status);
        assert_eq!(AudioSource::data(&frame), Some(samples.as_slice()));
        assert_eq!(
            frame.audio_descriptor().p_metadata.is_null(),
            status != FrameMetadataStatus::Text
        );
    });
    assert_eq!(allocations, 0);
    let mut owned = frame.owned_copy();
    let owned_status = if status == FrameMetadataStatus::Text {
        FrameMetadataStatus::Text
    } else {
        FrameMetadataStatus::Absent
    };
    assert_eq!(owned.metadata_status(), owned_status);
    assert_eq!(owned.data(), samples);
    frame.copy_into_owned(&mut owned);
    assert_eq!(owned.metadata_status(), owned_status);
    assert_eq!(
        owned.to_raw().p_metadata.is_null(),
        status != FrameMetadataStatus::Text
    );
    assert_eq!(FREES.with(Cell::get), 0);
    drop(frame);
    assert_eq!(FREES.with(Cell::get), 1);

    let invalid = NDIlib_audio_frame_v3_t {
        p_data: ptr::null_mut(),
        ..raw
    };
    // SAFETY: invalid media is rejected before reading samples; metadata stays live.
    let result = unsafe {
        AudioRef::<S>::from_guard(Guard::with_test_free(
            S::Instance::default(),
            invalid,
            count_free,
        ))
    };
    assert!(matches!(result, Err(Error::InvalidFrame(_))));
    assert_eq!(FREES.with(Cell::get), 2);

    #[cfg(feature = "advanced")]
    {
        // AV sync uses a borrowed descriptor rather than a receive Guard. It
        // shares the same metadata-state validation and forwarding policy.
        let (returned, allocations) = crate::test_alloc::allocation_count(|| unsafe {
            ValidatedAudio::returned_audio(&raw).unwrap()
        });
        assert_eq!(allocations, 0);
        let ReturnedAudio::Samples(returned) = returned else {
            panic!("samples")
        };
        assert_eq!(returned.metadata_state(), status);
        assert_eq!(
            returned.audio_descriptor().p_metadata.is_null(),
            status != FrameMetadataStatus::Text
        );
    }
}

#[test]
fn all_received_media_keep_their_payload_and_cache_metadata_status_without_allocating() {
    let unterminated = vec![b'x'; MAX_METADATA_BYTES];
    let cases = [
        (ptr::null(), FrameMetadataStatus::Absent),
        (c"".as_ptr(), FrameMetadataStatus::Text),
        (c"<valid/>".as_ptr(), FrameMetadataStatus::Text),
        (c"\xff".as_ptr(), FrameMetadataStatus::NotUtf8),
        (
            unterminated.as_ptr().cast(),
            FrameMetadataStatus::Unterminated,
        ),
    ];
    for (metadata, status) in cases {
        video::<VideoKind>(metadata, status);
        video::<FrameSyncVideoFree>(metadata, status);
        audio::<AudioKind>(metadata, status);
        audio::<FrameSyncAudioFree>(metadata, status);
    }
}

#[test]
fn alternating_metadata_copies_keep_storage_for_borrowed_owned_and_captured_sources() {
    let pixels = [37; 16];
    let samples = [0.25; 4];
    let present_video = crate::BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .metadata(c"<present/>")
        .build()
        .unwrap();
    let absent_video = crate::BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .build()
        .unwrap();
    let empty_video = crate::BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .metadata(c"")
        .build()
        .unwrap();
    let present_audio = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .metadata(c"<present/>")
        .build()
        .unwrap();
    let absent_audio = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .build()
        .unwrap();
    let empty_audio = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .metadata(c"")
        .build()
        .unwrap();
    let owned_video = present_video.to_owned();
    let bare_video = absent_video.to_owned();
    let owned_audio = present_audio.to_owned();
    let bare_audio = absent_audio.to_owned();
    // SAFETY: the input slices and metadata remain alive and immutable until
    // these guards are dropped; their callback releases no borrowed storage.
    let captured_video = unsafe {
        VideoFrameRef::new(Guard::with_test_free(
            ptr::null_mut(),
            present_video.video_descriptor(),
            count_free,
        ))
        .unwrap()
    };
    let captured_bare_video = unsafe {
        VideoFrameRef::new(Guard::with_test_free(
            ptr::null_mut(),
            absent_video.video_descriptor(),
            count_free,
        ))
        .unwrap()
    };
    let captured_audio = unsafe {
        AudioFrameRef::new(Guard::with_test_free(
            ptr::null_mut(),
            present_audio.audio_descriptor(),
            count_free,
        ))
        .unwrap()
    };
    let captured_bare_audio = unsafe {
        AudioFrameRef::new(Guard::with_test_free(
            ptr::null_mut(),
            absent_audio.audio_descriptor(),
            count_free,
        ))
        .unwrap()
    };
    let video_sources: [&dyn VideoSource; 8] = [
        &present_video,
        &absent_video,
        &owned_video,
        &bare_video,
        &captured_video,
        &captured_bare_video,
        &empty_video,
        &absent_video,
    ];
    let audio_sources: [&dyn AudioSource; 8] = [
        &present_audio,
        &absent_audio,
        &owned_audio,
        &bare_audio,
        &captured_audio,
        &captured_bare_audio,
        &empty_audio,
        &absent_audio,
    ];
    let mut video = VideoFrame::builder()
        .resolution(2, 2)
        .metadata("x".repeat(64))
        .build()
        .unwrap();
    let mut audio = AudioFrame::builder()
        .num_channels(2)
        .num_samples(2)
        .metadata("x".repeat(64))
        .build()
        .unwrap();
    let video_pointer = video.metadata().unwrap().as_ptr();
    let audio_pointer = audio.metadata().unwrap().as_ptr();
    let video_capacity = video.metadata.inner.capacity();
    let audio_capacity = audio.metadata.inner.capacity();
    #[cfg(feature = "advanced")]
    let retained = video.retained_bytes();
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        for _ in 0..10 {
            for (video_source, audio_source) in video_sources.into_iter().zip(audio_sources) {
                video_source.copy_into(&mut video);
                audio_source.copy_into(&mut audio);
                assert_eq!(video.metadata(), video_source.metadata());
                assert_eq!(audio.metadata(), audio_source.metadata());
                assert_eq!(
                    video.to_raw().p_metadata.is_null(),
                    video_source.metadata().is_none()
                );
                assert_eq!(
                    audio.to_raw().p_metadata.is_null(),
                    audio_source.metadata().is_none()
                );
                assert_eq!(video.metadata.inner.capacity(), video_capacity);
                assert_eq!(audio.metadata.inner.capacity(), audio_capacity);
                if video_source.metadata().is_some() {
                    assert_eq!(video.metadata().unwrap().as_ptr(), video_pointer);
                    assert_eq!(audio.metadata().unwrap().as_ptr(), audio_pointer);
                }
                #[cfg(feature = "advanced")]
                assert_eq!(video.retained_bytes(), retained);
            }
            video.clear_metadata();
            audio.clear_metadata();
            assert_eq!(video.metadata.inner.capacity(), video_capacity);
            assert_eq!(audio.metadata.inner.capacity(), audio_capacity);
        }
    });
    assert_eq!(allocations, 0);
}
