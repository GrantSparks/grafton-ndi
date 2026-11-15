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
        assert_eq!(VideoParts::frame_metadata(&frame).status(), status);
        assert_eq!(VideoSource::metadata_status(&frame), status);
        if matches!(
            status,
            FrameMetadataStatus::Absent | FrameMetadataStatus::Text
        ) {
            assert_eq!(VideoSource::color_info(&frame).unwrap(), None);
        }
        assert_eq!(VideoSource::data(&frame), Some(pixels.as_slice()));
        assert_eq!(
            Sendable::for_test(&frame)
                .video_descriptor()
                .p_metadata
                .is_null(),
            status != FrameMetadataStatus::Text
        );
    });
    assert_eq!(allocations, 0);
    let (mut owned, allocations) = crate::test_alloc::allocation_count(|| frame.owned_copy());
    assert_eq!(
        allocations,
        1 + usize::from(status == FrameMetadataStatus::Text)
    );
    assert_eq!(owned.metadata_status(), status);
    let (cloned, allocations) = crate::test_alloc::allocation_count(|| owned.clone());
    assert_eq!(
        allocations,
        1 + usize::from(status == FrameMetadataStatus::Text)
    );
    assert_eq!(cloned.metadata_status(), status);
    let copy = VideoSource::to_owned(&owned);
    assert_eq!(copy.metadata_status(), status);
    assert_eq!(owned.data(), pixels);
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        for _ in 0..3 {
            frame.copy_into_owned(&mut owned);
            assert_eq!(owned.metadata_status(), status);
            copy.copy_into_owned(&mut owned);
            assert_eq!(owned.metadata_status(), status);
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(
        owned.as_test_raw().p_metadata.is_null(),
        status != FrameMetadataStatus::Text
    );
    owned
        .set_metadata("replacement metadata with capacity to reuse")
        .unwrap();
    let metadata_capacity = owned.metadata.retained_bytes();
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        frame.copy_into_owned(&mut owned);
        assert_eq!(owned.metadata_status(), status);
        assert_eq!(owned.metadata.retained_bytes(), metadata_capacity);
    });
    assert_eq!(allocations, 0);
    assert!(owned.set_metadata("bad\0text").is_err());
    assert_eq!(owned.metadata_status(), status);
    owned.set_metadata("").unwrap();
    assert_eq!(owned.metadata_status(), FrameMetadataStatus::Text);
    assert_eq!(owned.metadata(), Some(""));
    owned.clear_metadata();
    assert_eq!(owned.metadata_status(), FrameMetadataStatus::Absent);
    assert_eq!(owned.metadata(), None);
    frame.copy_into_owned(&mut owned);
    assert_eq!(owned.metadata_status(), status);
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
        assert_eq!(AudioParts::frame_metadata(&frame).status(), status);
        assert_eq!(AudioSource::metadata_status(&frame), status);
        assert_eq!(AudioSource::data(&frame), Some(samples.as_slice()));
        assert_eq!(
            frame.audio_descriptor().p_metadata.is_null(),
            status != FrameMetadataStatus::Text
        );
    });
    assert_eq!(allocations, 0);
    let (mut owned, allocations) = crate::test_alloc::allocation_count(|| frame.owned_copy());
    assert_eq!(
        allocations,
        1 + usize::from(status == FrameMetadataStatus::Text)
    );
    assert_eq!(owned.metadata_status(), status);
    let (cloned, allocations) = crate::test_alloc::allocation_count(|| owned.clone());
    assert_eq!(
        allocations,
        1 + usize::from(status == FrameMetadataStatus::Text)
    );
    assert_eq!(cloned.metadata_status(), status);
    let copy = AudioSource::to_owned(&owned);
    assert_eq!(copy.metadata_status(), status);
    assert_eq!(owned.data(), samples);
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        for _ in 0..3 {
            frame.copy_into_owned(&mut owned);
            assert_eq!(owned.metadata_status(), status);
            copy.copy_into_owned(&mut owned);
            assert_eq!(owned.metadata_status(), status);
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(
        owned.audio_descriptor().p_metadata.is_null(),
        status != FrameMetadataStatus::Text
    );
    owned
        .set_metadata("replacement metadata with capacity to reuse")
        .unwrap();
    let metadata_capacity = owned.metadata.retained_bytes();
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        frame.copy_into_owned(&mut owned);
        assert_eq!(owned.metadata_status(), status);
        assert_eq!(owned.metadata.retained_bytes(), metadata_capacity);
    });
    assert_eq!(allocations, 0);
    assert!(owned.set_metadata("bad\0text").is_err());
    assert_eq!(owned.metadata_status(), status);
    owned.set_metadata("").unwrap();
    assert_eq!(owned.metadata_status(), FrameMetadataStatus::Text);
    assert_eq!(owned.metadata(), Some(""));
    owned.clear_metadata();
    assert_eq!(owned.metadata_status(), FrameMetadataStatus::Absent);
    assert_eq!(owned.metadata(), None);
    frame.copy_into_owned(&mut owned);
    assert_eq!(owned.metadata_status(), status);
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
            ValidatedAudio::returned_audio_descriptor(&raw).unwrap()
        });
        assert_eq!(allocations, 0);
        let ReturnedAudio::Samples(returned) = returned else {
            panic!("samples")
        };
        assert_eq!(returned.frame_metadata().status(), status);
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
fn withheld_video_color_is_refused_on_receiver_framesync_and_owned_frames() {
    let pq = b"<ndi_color_info transfer=\"bt_2100_pq\" matrix=\"bt_2020\"/>";
    let mut unterminated = vec![b'x'; MAX_METADATA_BYTES];
    unterminated[..pq.len()].copy_from_slice(pq);
    let mut pixels = [37; 32];
    for (metadata, status) in [
        (
            c"<ndi_color_info transfer=\"bt_2100_pq\" matrix=\"bt_2020\"/>\xff".as_ptr(),
            FrameMetadataStatus::NotUtf8,
        ),
        (
            unterminated.as_ptr().cast(),
            FrameMetadataStatus::Unterminated,
        ),
    ] {
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
        macro_rules! check {
            ($kind:ty) => {{
                // SAFETY: the pixels and bounded metadata buffers remain live
                // and unchanged until the guard drops; release only counts.
                let frame = unsafe {
                    VideoRef::<$kind>::new(Guard::with_test_free(ptr::null_mut(), raw, count_free))
                        .unwrap()
                };
                let mut owned = frame.to_owned();
                for source in [&frame as &dyn VideoSource, &owned] {
                    assert_eq!(source.metadata(), None);
                    assert_eq!(source.metadata_status(), status);
                    let Error::InvalidFrame(message) = source.color_info().unwrap_err() else {
                        panic!("expected an invalid color description")
                    };
                    assert!(matches!(message, crate::FrameDefect::ColorMetadataWithheld {status:found} if found==status), "{message}");

                }
                assert!(matches!(frame.color_info(), Err(Error::InvalidFrame(_))));
                assert!(matches!(owned.color_info(), Err(Error::InvalidFrame(_))));
                #[cfg(feature = "image-encoding")]
                {
                    assert!(matches!(frame.encode_png(), Err(Error::InvalidFrame(_))));
                    assert!(matches!(owned.encode_png(), Err(Error::InvalidFrame(_))));
                    let options = crate::ImageOptions::new(crate::ImageFormat::Png).color(
                        crate::ColorPolicy::Override {
                            matrix: crate::YuvMatrix::Bt709,
                            range: crate::YuvRange::Limited,
                        },
                    );
                    assert_eq!(
                        frame.encode_image(&options).unwrap(),
                        owned.encode_image(&options).unwrap()
                    );
                    let mut workspace = crate::ImageWorkspace::new();
                    let mut output = Vec::new();
                    for source in [&frame as &dyn VideoSource, &owned] {
                        assert!(matches!(
                            workspace.encode_to(
                                source,
                                &crate::ImageOptions::new(crate::ImageFormat::Png),
                                &mut output,
                            ),
                            Err(Error::InvalidFrame(_))
                        ));
                        assert!(output.is_empty());
                        assert_eq!(workspace.retained_bytes(), 0);
                        workspace.encode(source, &options).unwrap();
                        workspace.release();
                    }
                }
                owned.clear_metadata();
                assert_eq!(owned.color_info().unwrap(), None);
                frame.copy_into(&mut owned);
                assert_eq!(owned.metadata_status(), status);
                assert!(matches!(owned.color_info(), Err(Error::InvalidFrame(_))));
                owned
                    .set_metadata("<ndi_color_info matrix='bt_601'/>")
                    .unwrap();
                assert_eq!(owned.metadata_status(), FrameMetadataStatus::Text);
                assert_eq!(
                    owned.color_info().unwrap().unwrap().matrix(),
                    Some(crate::YuvMatrix::Bt601)
                );
            }};
        }
        check!(VideoKind);
        check!(FrameSyncVideoFree);
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
    let captured_video = VideoFrameRef::new(unsafe {
        Guard::with_test_free(
            ptr::null_mut(),
            Sendable::for_test(&present_video).video_descriptor(),
            count_free,
        )
    })
    .unwrap();
    let captured_bare_video = VideoFrameRef::new(unsafe {
        Guard::with_test_free(
            ptr::null_mut(),
            Sendable::for_test(&absent_video).video_descriptor(),
            count_free,
        )
    })
    .unwrap();
    let captured_audio = AudioFrameRef::new(unsafe {
        Guard::with_test_free(
            ptr::null_mut(),
            present_audio.audio_descriptor(),
            count_free,
        )
    })
    .unwrap();
    let captured_bare_audio = AudioFrameRef::new(unsafe {
        Guard::with_test_free(ptr::null_mut(), absent_audio.audio_descriptor(), count_free)
    })
    .unwrap();
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
    let video_capacity = video.metadata.retained_bytes();
    let audio_capacity = audio.metadata.retained_bytes();
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
                    video.as_test_raw().p_metadata.is_null(),
                    video_source.metadata().is_none()
                );
                assert_eq!(
                    audio.audio_descriptor().p_metadata.is_null(),
                    audio_source.metadata().is_none()
                );
                assert_eq!(video.metadata.retained_bytes(), video_capacity);
                assert_eq!(audio.metadata.retained_bytes(), audio_capacity);
                if video_source.metadata().is_some() {
                    assert_eq!(video.metadata().unwrap().as_ptr(), video_pointer);
                    assert_eq!(audio.metadata().unwrap().as_ptr(), audio_pointer);
                }
                #[cfg(feature = "advanced")]
                assert_eq!(video.retained_bytes(), retained);
            }
            video.clear_metadata();
            audio.clear_metadata();
            assert_eq!(video.metadata.retained_bytes(), video_capacity);
            assert_eq!(audio.metadata.retained_bytes(), audio_capacity);
        }
    });
    assert_eq!(allocations, 0);
}

#[test]
fn audio_timestamp_normalization_depends_on_the_capture_family() {
    fn check<S>(ticks: i64, expected: Option<SdkTimestamp>)
    where
        S: FrameFree<RawFrame = NDIlib_audio_frame_v3_t>,
        S::Instance: Default,
    {
        let mut samples = [0.25f32; 4];
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 2,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: samples.as_mut_ptr().cast(),
            timestamp: ticks,
            timecode: Timecode::SYNTHESIZE.ticks(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 8,
            },
            ..Default::default()
        };
        // SAFETY: the guard borrows these live initialized samples; release
        // only updates the counter and does not free their borrowed storage.
        let frame = unsafe {
            AudioRef::<S>::from_guard(Guard::with_test_free(
                S::Instance::default(),
                raw,
                count_free,
            ))
            .unwrap()
        };
        assert_eq!(frame.sdk_timestamp(), expected);
        let mut owned = frame.owned_copy();
        assert_eq!(owned.timestamp(), expected);
        assert_eq!(owned.timecode(), Timecode::SYNTHESIZE);
        frame.copy_into_owned(&mut owned);
        assert_eq!(owned.timestamp(), expected);
    }
    for ticks in [0, NDIlib_recv_timestamp_undefined, 123_456] {
        check::<AudioKind>(ticks, SdkTimestamp::from_sdk(ticks));
        check::<FrameSyncAudioFree>(
            ticks,
            if ticks == 0 {
                None
            } else {
                SdkTimestamp::from_sdk(ticks)
            },
        );
    }
}

#[test]
fn owned_metadata_states_keep_one_observation_and_reuse_their_storage() {
    let present = MetadataText::from_cstr(c"<present/>").unwrap();
    let empty = MetadataText::from_cstr(c"").unwrap();
    let observations = [
        FrameMetadataRef::Absent,
        FrameMetadataRef::Text(present),
        FrameMetadataRef::Withheld(MetadataWithheld::NotUtf8),
        FrameMetadataRef::Text(empty),
        FrameMetadataRef::Withheld(MetadataWithheld::Unterminated),
    ];
    let mut owned = OwnedFrameMetadata::new("x".repeat(64)).unwrap();
    let capacity = owned.retained_bytes();
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        for observation in observations.into_iter().cycle().take(25) {
            owned.copy_from(observation);
            assert_eq!(owned.view().status(), observation.status());
            assert_eq!(
                owned.view().text().map(MetadataText::as_str),
                observation.text().map(MetadataText::as_str)
            );
            assert_eq!(owned.retained_bytes(), capacity);
            assert_eq!(
                owned.view().as_ptr().is_null(),
                observation.text().is_none()
            );
        }
    });
    assert_eq!(allocations, 0);
    let status = owned.view().status();
    assert!(owned.set("bad\0text").is_err());
    assert_eq!(owned.view().status(), status);
    owned.clear();
    assert_eq!(owned.view().status(), FrameMetadataStatus::Absent);
    assert_eq!(owned.retained_bytes(), capacity);
}

#[test]
fn metadata_constructors_seal_the_sole_terminator_and_reuse_storage() {
    use super::metadata::MetadataText;
    assert_eq!(MetadataText::EMPTY.as_cstr().to_bytes_with_nul(), b"\0");
    for bytes in [b"missing".as_slice(), b"a\0b\0", b"\xff\0"] {
        assert!(MetadataText::from_terminated_bytes(bytes).is_err());
    }
    let text = MetadataText::from_terminated_bytes(b"<a/>\0").unwrap();
    assert_eq!(text.as_str(), "<a/>");
    let mut destination = OwnedFrameMetadata::Absent(String::with_capacity(64));
    let capacity = destination.retained_bytes();
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        destination.copy_from(FrameMetadataRef::Text(text));
    });
    assert_eq!(allocations, 0);
    assert_eq!(destination.retained_bytes(), capacity);
    assert_eq!(destination.view().text().unwrap().as_str(), "<a/>");
}

fn expect_original_metadata(
    text: String,
    operation: impl FnOnce(String) -> std::result::Result<(), Rejected<String>>,
) {
    let pointer = text.as_ptr();
    let capacity = text.capacity();
    let oversized = text.len() == MAX_METADATA_BYTES;
    let (result, allocations) = crate::test_alloc::allocation_count(|| operation(text));
    assert_eq!(allocations, if oversized { 1 } else { 2 });
    let (error, text) = result.unwrap_err().into_parts();
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(text.capacity(), capacity);
    if oversized {
        assert!(matches!(
            error,
            Error::InvalidFrame(crate::FrameDefect::MetadataTooLarge { .. })
        ));
        assert_eq!(text.len(), MAX_METADATA_BYTES);
    } else {
        let Error::InvalidCString(error) = error else {
            panic!("expected interior NUL")
        };
        assert_eq!(error.nul_position(), 5);
        assert_eq!(error.into_vec(), b"<ok/>\0");
        assert_eq!(text, "<ok/>\0tail");
    }
}

#[test]
fn every_owned_metadata_entrypoint_returns_the_original_text_and_keeps_the_frame() {
    let mut metadata = MetadataFrame::new("previous").unwrap();
    let mut video = VideoFrame::builder()
        .resolution(2, 2)
        .metadata("previous")
        .build()
        .unwrap();
    let mut audio = AudioFrame::builder().metadata("previous").build().unwrap();
    for text in ["<ok/>\0tail".to_owned(), "x".repeat(MAX_METADATA_BYTES)] {
        expect_original_metadata(text.clone(), |text| MetadataFrame::new(text).map(drop));
        expect_original_metadata(text.clone(), |text| metadata.set_data(text));
        expect_original_metadata(text.clone(), |text| video.set_metadata(text));
        expect_original_metadata(text, |text| audio.set_metadata(text));
        assert_eq!(metadata.data(), "previous");
        assert_eq!(video.metadata(), Some("previous"));
        assert_eq!(audio.metadata(), Some("previous"));
        assert_eq!(video.metadata_status(), FrameMetadataStatus::Text);
        assert_eq!(audio.metadata_status(), FrameMetadataStatus::Text);
    }
}

#[test]
fn rejected_owned_builders_expose_metadata_for_repair_without_copying() {
    let text = "bad\0text".to_owned();
    let pointer = text.as_ptr();
    let capacity = text.capacity();
    let mut builder = VideoFrame::builder()
        .resolution(2, 2)
        .metadata(text)
        .build()
        .unwrap_err()
        .into_inner();
    assert_eq!(builder.supplied_metadata(), Some("bad\0text"));
    let mut text = builder.take_metadata().unwrap();
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(text.capacity(), capacity);
    text.remove(3);
    assert_eq!(
        builder.metadata(text).build().unwrap().metadata(),
        Some("badtext")
    );

    let text = "bad\0text".to_owned();
    let pointer = text.as_ptr();
    let capacity = text.capacity();
    let mut builder = AudioFrame::builder()
        .metadata(text)
        .build()
        .unwrap_err()
        .into_inner();
    assert_eq!(builder.supplied_metadata(), Some("bad\0text"));
    let mut text = builder.take_metadata().unwrap();
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(text.capacity(), capacity);
    text.remove(3);
    assert_eq!(
        builder.metadata(text).build().unwrap().metadata(),
        Some("badtext")
    );
}
