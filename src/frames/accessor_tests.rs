use super::*;

#[macro_use]
#[path = "../../tests/support/source_surface.rs"]
mod surface;

#[test]
fn source_traits_require_every_method_without_default_bodies() {
    let syntax = syn::parse_file(include_str!("../frames.rs")).unwrap();
    for name in ["VideoSource", "AudioSource", "MetadataSource"] {
        let declaration = syntax
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Trait(item) if item.ident == name => Some(item),
                _ => None,
            })
            .unwrap();
        for item in &declaration.items {
            let syn::TraitItem::Fn(method) = item else {
                panic!("unexpected non-method in {name}");
            };
            assert!(
                method.default.is_none(),
                "{name}::{} has a second accessor body",
                method.sig.ident
            );
        }
    }
}

#[test]
fn owned_trait_copy_keeps_the_same_retained_video_room_as_its_inherent_clone() {
    let mut frame = VideoFrame::builder()
        .resolution(4, 6)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();
    frame.set_scan_type(ScanType::Interlaced);
    frame.set_scan_type(ScanType::Progressive);
    let direct = frame.to_owned();
    let erased = VideoSource::to_owned(&frame);
    assert_eq!(direct.data.room(), frame.data.room());
    assert_eq!(erased.data.room(), frame.data.room());
    assert_eq!(direct.data(), erased.data());
}

#[test]
fn captured_video_surfaces_agree_for_packed_and_padded_rows() {
    for stride in [8, 12] {
        let mut pixels = vec![128u8; stride * 2];
        let raw = NDIlib_video_frame_v2_t {
            xres: 2,
            yres: 2,
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            frame_rate_N: 25,
            frame_rate_D: 1,
            picture_aspect_ratio: 1.0,
            frame_format_type: ScanType::Progressive.to_sdk(),
            p_data: pixels.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
                line_stride_in_bytes: stride as i32,
            },
            p_metadata: c"<frame/>".as_ptr(),
            timecode: 123,
            timestamp: 456,
        };
        // SAFETY: both guards have no-op frees; these initialized pixels and
        // static metadata outlive both checked references.
        let (receiver, synchronizer) = unsafe {
            (
                VideoFrameRef::new(Guard::<VideoKind>::with_test_free(
                    std::ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
                .unwrap(),
                FrameSyncVideoRef::new(Guard::<FrameSyncVideoFree>::with_test_free(
                    std::ptr::null_mut(),
                    raw,
                    |_, _| {},
                ))
                .unwrap(),
            )
        };
        video_source_surface!(&receiver, receiver.data(), VideoSource, VideoFrame);
        video_source_surface!(&synchronizer, synchronizer.data(), VideoSource, VideoFrame);
    }
}

#[test]
fn captured_audio_surfaces_agree_for_packed_and_padded_channels() {
    for stride in [2, 3] {
        let mut samples = vec![0.0; stride * 2];
        samples[..2].copy_from_slice(&[1.0, 2.0]);
        samples[stride..stride + 2].copy_from_slice(&[3.0, 4.0]);
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 2,
            FourCC: AudioFormat::Fltp.to_sdk(),
            p_data: samples.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: (stride * 4) as i32,
            },
            p_metadata: c"<frame/>".as_ptr(),
            timecode: 123,
            timestamp: 456,
        };
        // SAFETY: initialized samples and static metadata outlive each no-op
        // test loan. Each reference validates its delivered capture descriptor.
        let receiver = unsafe {
            AudioFrameRef::new(RecvAudioGuard::with_test_free(
                std::ptr::null_mut(),
                raw,
                |_, _| {},
            ))
        }
        .unwrap();
        // SAFETY: the same live initialized storage backs this independent loan.
        let synchronizer = unsafe {
            FrameSyncAudioRef::new(Guard::<FrameSyncAudioFree>::with_test_free(
                std::ptr::null_mut(),
                raw,
                |_, _| {},
            ))
        }
        .unwrap();
        audio_source_surface!(&receiver, receiver.data(), AudioSource, AudioFrame);
        audio_source_surface!(&synchronizer, synchronizer.data(), AudioSource, AudioFrame);
        #[cfg(feature = "advanced")]
        {
            // SAFETY: this synthetic no-op loan borrows that same live storage.
            let avsync = unsafe {
                ValidatedAudio::audio(Guard::<AvSyncAudioFree>::with_test_free(
                    crate::capture::AvSyncAudioInstance::Synthetic(std::ptr::null()),
                    raw,
                    |_, _| {},
                ))
            }
            .unwrap();
            let avsync = AvSyncAudioRef::from_validated(avsync);
            audio_source_surface!(&avsync, avsync.data(), AudioSource, AudioFrame);
        }
    }
}

#[test]
fn metadata_surfaces_forward_without_losing_empty_text_or_timecodes() {
    use crate::MetadataSource;
    for text in [c"", c"<frame/>"] {
        let raw = NDIlib_metadata_frame_t {
            length: text.to_bytes_with_nul().len() as i32,
            timecode: 789,
            p_data: text.as_ptr().cast_mut(),
        };
        // SAFETY: static text outlives this no-op test loan.
        let frame = unsafe {
            MetadataFrameRef::new(RecvMetadataGuard::with_test_free(
                std::ptr::null_mut(),
                raw,
                |_, _| {},
            ))
            .unwrap()
        };
        let owned = frame.to_owned();
        for source in [&frame as &dyn MetadataSource, &owned as &dyn MetadataSource] {
            assert_eq!(source.data(), frame.data());
            assert_eq!(source.as_bytes(), frame.as_bytes());
            assert_eq!(source.timecode(), frame.timecode());
            assert_eq!(source.to_owned().data(), owned.data());
            let mut destination = MetadataFrame::new("different").unwrap();
            source.copy_into(&mut destination);
            assert_eq!(destination.data(), owned.data());
            assert_eq!(destination.timecode(), owned.timecode());
        }
    }
}
