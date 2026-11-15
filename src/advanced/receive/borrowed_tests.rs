use super::*;
use crate::{
    advanced::{EnvelopeCodec, PacketEnvelope, VideoPacketFormat},
    FrameMetadataStatus, SdkTimestamp,
};
use std::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn borrowed_compressed_payloads_allocate_nothing_and_release_once() {
    let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
    let mut source = CompressedVideoFrame::new(
        format,
        PacketEnvelope::encode(EnvelopeCodec::H264, 1, 0, true, &[1, 2, 3], &[4]).unwrap(),
    )
    .unwrap();
    source.set_metadata("<packet/>").unwrap();
    let raw = source.raw();
    let frees = AtomicUsize::new(0);
    let instance = (&frees as *const AtomicUsize).cast_mut().cast();
    let guard = unsafe {
        Guard::with_test_free(instance, raw, |instance, _| {
            (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
        })
    };
    let (item, allocations) =
        crate::test_alloc::allocation_count(|| video_ref_event(guard).unwrap());
    assert_eq!(allocations, 0);
    assert_eq!(frees.load(Ordering::Relaxed), 0);
    let PacketItemRef::Video(ReceivedVideoRef::Compressed(frame)) = &item else {
        panic!("compressed")
    };
    assert_eq!(frame.data().as_ptr(), source.data().as_ptr());
    assert_eq!(frame.metadata(), Some("<packet/>"));
    assert_eq!(frame.metadata_status(), FrameMetadataStatus::Text);
    let (mut owned, allocations) = crate::test_alloc::allocation_count(|| frame.to_owned());
    assert_eq!(allocations, 2);
    assert_eq!(owned.data(), frame.data());
    let pointer = owned.data().as_ptr();
    let (_, allocations) = crate::test_alloc::allocation_count(|| frame.copy_into(&mut owned));
    assert_eq!(allocations, 0);
    assert_eq!(owned.data().as_ptr(), pointer);
    let raw_bare = NDIlib_video_frame_v2_t {
        p_metadata: ptr::null(),
        ..raw
    };
    let bare = CompressedVideoRef::new(unsafe {
        Guard::with_test_free(ptr::null_mut(), raw_bare, |_, _| {})
    })
    .unwrap();
    let (_, allocations) = crate::test_alloc::allocation_count(|| {
        bare.copy_into(&mut owned);
        assert_eq!(owned.metadata(), None);
        frame.copy_into(&mut owned);
    });
    assert_eq!(allocations, 0);
    assert_eq!(owned.metadata(), Some("<packet/>"));
    drop(item);
    assert_eq!(frees.load(Ordering::Relaxed), 1);

    let failed = std::panic::catch_unwind(|| {
        let guard = unsafe {
            Guard::with_test_free(instance, raw, |instance, _| {
                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
            })
        };
        let _frame = video_ref_event(guard).unwrap();
        panic!("application processing panic");
    });
    assert!(failed.is_err());
    assert_eq!(frees.load(Ordering::Relaxed), 2);
    let invalid = NDIlib_video_frame_v2_t {
        __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
            data_size_in_bytes: 1,
        },
        ..raw
    };
    let guard = unsafe {
        Guard::with_test_free(instance, invalid, |instance, _| {
            (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
        })
    };
    assert!(video_ref_event(guard).is_err());
    assert_eq!(frees.load(Ordering::Relaxed), 3);

    let unterminated = vec![b'x'; crate::frames::MAX_METADATA_BYTES];
    for (pointer, status) in [
        (ptr::null(), FrameMetadataStatus::Absent),
        (c"\xff".as_ptr(), FrameMetadataStatus::NotUtf8),
        (
            unterminated.as_ptr().cast(),
            FrameMetadataStatus::Unterminated,
        ),
    ] {
        let raw = NDIlib_video_frame_v2_t {
            p_metadata: pointer,
            ..raw
        };
        let guard = unsafe { Guard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
        let (frame, allocations) =
            crate::test_alloc::allocation_count(|| CompressedVideoRef::new(guard).unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(frame.metadata_status(), status);
        assert_eq!(frame.metadata(), None);
        let copied = frame.to_owned();
        assert_eq!(copied.metadata(), None);
        assert_eq!(copied.metadata_status(), status);
        assert_eq!(copied.clone().metadata_status(), status);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            frame.copy_into(&mut owned);
            assert_eq!(owned.metadata_status(), status);
            assert_eq!(owned.metadata(), None);
        });
        assert_eq!(allocations, 0);
        owned.set_metadata("").unwrap();
        assert_eq!(owned.metadata_status(), FrameMetadataStatus::Text);
        owned.clear_metadata();
        assert_eq!(owned.metadata_status(), FrameMetadataStatus::Absent);
    }
}

#[test]
fn borrowed_audio_uses_receive_validation_and_reuses_owned_storage() {
    let mut source =
        CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1, 2, 3, 4]).unwrap();
    source.set_metadata("<audio/>").unwrap();
    let raw = source.raw();
    let frees = AtomicUsize::new(0);
    let instance = (&frees as *const AtomicUsize).cast_mut().cast();
    let guard = unsafe {
        Guard::with_test_free(instance, raw, |instance, _| {
            (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
        })
    };
    let (item, allocations) =
        crate::test_alloc::allocation_count(|| audio_ref_event(guard).unwrap());
    assert_eq!(allocations, 0);
    let PacketItemRef::Audio(ReceivedAudioRef::Compressed(frame)) = &item else {
        panic!("compressed")
    };
    assert_eq!(frame.data().as_ptr(), source.data().as_ptr());
    assert_eq!(
        (
            frame.sample_rate(),
            frame.num_channels(),
            frame.num_samples()
        ),
        (48_000, 2, 960)
    );
    let (mut copy, allocations) = crate::test_alloc::allocation_count(|| frame.to_owned());
    assert_eq!(allocations, 2);
    let (_, allocations) = crate::test_alloc::allocation_count(|| frame.copy_into(&mut copy));
    assert_eq!(allocations, 0);
    assert_eq!(copy.data(), source.data());
    assert_eq!(copy.metadata(), source.metadata());
    assert_eq!(frees.load(Ordering::Relaxed), 0);
    drop(item);
    assert_eq!(frees.load(Ordering::Relaxed), 1);

    let invalid = NDIlib_audio_frame_v3_t {
        no_samples: 0,
        ..raw
    };
    let guard = unsafe {
        Guard::with_test_free(instance, invalid, |instance, _| {
            (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
        })
    };
    assert!(audio_ref_event(guard).is_err());
    assert_eq!(frees.load(Ordering::Relaxed), 2);
    assert!(std::panic::catch_unwind(|| {
        let guard = unsafe {
            Guard::with_test_free(instance, raw, |instance, _| {
                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::Relaxed);
            })
        };
        let _frame = audio_ref_event(guard).unwrap();
        panic!("application processing panic");
    })
    .is_err());
    assert_eq!(frees.load(Ordering::Relaxed), 3);

    let unterminated = vec![b'x'; crate::frames::MAX_METADATA_BYTES];
    for (pointer, status, text) in [
        (ptr::null(), FrameMetadataStatus::Absent, None),
        (c"".as_ptr(), FrameMetadataStatus::Text, Some("")),
        (
            c"<audio/>".as_ptr(),
            FrameMetadataStatus::Text,
            Some("<audio/>"),
        ),
        (c"\xff".as_ptr(), FrameMetadataStatus::NotUtf8, None),
        (
            unterminated.as_ptr().cast(),
            FrameMetadataStatus::Unterminated,
            None,
        ),
    ] {
        let raw = NDIlib_audio_frame_v3_t {
            p_metadata: pointer,
            ..raw
        };
        let frame = CompressedAudioRef::new(unsafe {
            Guard::with_test_free(ptr::null_mut(), raw, |_, _| {})
        })
        .unwrap();
        assert_eq!(frame.metadata(), text);
        assert_eq!(frame.metadata_status(), status);
        let owned = frame.to_owned();
        assert_eq!(owned.metadata(), text);
        assert_eq!(owned.metadata_status(), status);
        assert_eq!(owned.clone().metadata_status(), status);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            frame.copy_into(&mut copy);
            assert_eq!(copy.metadata(), text);
            assert_eq!(copy.metadata_status(), status);
        });
        assert_eq!(allocations, 0);
    }
}

#[test]
fn decoded_packet_fallback_borrows_the_original_pixels_without_allocating() {
    let source = VideoFrame::builder()
        .resolution(4, 2)
        .metadata("<decoded/>")
        .build()
        .unwrap();
    let raw = source.as_test_raw();
    let guard = unsafe { Guard::with_test_free(ptr::null_mut(), raw, |_, _| {}) };
    let (item, allocations) =
        crate::test_alloc::allocation_count(|| video_ref_event(guard).unwrap());
    assert_eq!(allocations, 0);
    let PacketItemRef::Video(ReceivedVideoRef::Decoded(frame)) = item else {
        panic!("decoded")
    };
    assert_eq!(frame.data().unwrap().as_ptr(), source.data().as_ptr());
    assert_eq!(frame.metadata(), source.metadata());
}

#[test]
fn borrowed_packets_normalize_timestamps_through_their_held_capture() {
    let format = VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap();
    let mut video = CompressedVideoFrame::new(
        format,
        PacketEnvelope::encode(EnvelopeCodec::H264, 1, 0, true, &[1, 2, 3], &[4]).unwrap(),
    )
    .unwrap();
    let mut audio =
        CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1, 2, 3, 4]).unwrap();
    for (ticks, expected) in [
        (NDIlib_recv_timestamp_undefined, None),
        (0, Some(0)),
        (123, Some(123)),
    ] {
        let raw_video = NDIlib_video_frame_v2_t {
            timestamp: ticks,
            ..video.raw()
        };
        let raw_audio = NDIlib_audio_frame_v3_t {
            timestamp: ticks,
            ..audio.raw()
        };
        // SAFETY: both source frames retain their immutable packet allocations;
        // these synthetic guards only count the lifetime and free nothing.
        let video_ref = CompressedVideoRef::new(unsafe {
            Guard::with_test_free(ptr::null_mut(), raw_video, |_, _| {})
        })
        .unwrap();
        let audio_ref = CompressedAudioRef::new(unsafe {
            Guard::with_test_free(ptr::null_mut(), raw_audio, |_, _| {})
        })
        .unwrap();
        assert_eq!(video_ref.timestamp().map(SdkTimestamp::ticks), expected);
        assert_eq!(audio_ref.timestamp().map(SdkTimestamp::ticks), expected);
        assert_eq!(
            video_ref.to_owned().timestamp().map(SdkTimestamp::ticks),
            expected
        );
        assert_eq!(
            audio_ref.to_owned().timestamp().map(SdkTimestamp::ticks),
            expected
        );
    }
}
