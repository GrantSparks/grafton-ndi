//! Application-side generic and type-erased media inspection and storage reuse.
use grafton_ndi::{
    AudioFrame, AudioSource, BorrowedAudioFrame, BorrowedVideoFrame, FrameRate, PixelFormat,
    ScanType, Timecode, VideoFrame, VideoSource,
};

fn inspect_audio(source: &(impl AudioSource + ?Sized)) {
    assert_eq!(source.sample_rate(), 96_000);
    assert_eq!(source.num_channels(), 2);
    assert_eq!(source.num_samples(), 2);
    assert_eq!(source.channel_stride_bytes(), 12);
    assert_eq!(source.timecode(), Timecode::from_ticks(123));
    assert_eq!(source.timestamp(), None);
    assert_eq!(source.metadata(), Some(""));
    assert!(source.data().is_none());
    assert_eq!(source.channel_data(0), Some(&[1.0, 2.0][..]));
    assert_eq!(source.channels().next_back(), Some(&[3.0, 4.0][..]));
    assert_eq!(source.channels().len(), 2);
    assert!(source.channel_data(2).is_none());
    let owned = source.to_owned();
    assert_eq!(owned.data(), &[1.0, 2.0, 3.0, 4.0]);
    assert_eq!(owned.metadata(), Some(""));
    assert_eq!(owned.timecode(), source.timecode());
    let mut destination = AudioFrame::builder()
        .num_channels(2)
        .num_samples(16)
        .build()
        .unwrap();
    let storage = destination.data().as_ptr();
    source.copy_into(&mut destination);
    assert_eq!(destination.data().as_ptr(), storage);
    assert_eq!(destination.data(), owned.data());
}

#[test]
fn borrowed_planar_audio_can_be_inspected_generically_and_erased() {
    let samples = [1.0, 2.0, f32::NAN, 3.0, 4.0, 987.0];
    let frame = BorrowedAudioFrame::builder(&samples, 96_000, 2, 2)
        .channel_stride_bytes(12)
        .metadata(c"")
        .timecode(Timecode::from_ticks(123))
        .build()
        .unwrap();
    assert_eq!(frame.channel_data(0).unwrap().as_ptr(), samples.as_ptr());
    inspect_audio(&frame);
    inspect_audio(&frame as &dyn AudioSource);
    std::thread::scope(|scope| {
        let channels = frame.channels();
        scope.spawn(move || assert_eq!(channels.map(|x| x.len()).sum::<usize>(), 4));
    });
}

fn inspect_video(source: &(impl VideoSource + ?Sized)) {
    assert_eq!(source.width(), 2);
    assert_eq!(source.height(), 2);
    assert_eq!(source.line_stride_bytes(), 12);
    assert_eq!(source.pixel_format(), PixelFormat::Bgra);
    assert_eq!(source.frame_rate(), FrameRate::new(25, 1).unwrap());
    assert_eq!(source.scan_type(), ScanType::Progressive);
    assert_eq!(source.picture_aspect_ratio(), 0.0);
    assert_eq!(source.timecode(), Timecode::from_ticks(456));
    assert_eq!(source.timestamp(), None);
    assert_eq!(source.metadata(), Some("<video/>"));
    assert_eq!(source.planes().len(), 1);
    assert_eq!(source.plane(0).unwrap().row(1), Some(&[2; 8][..]));
    assert!(source.data().is_none());
    let owned = source.to_owned();
    assert_eq!(
        owned.data(),
        &[1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2]
    );
    let mut destination = VideoFrame::builder().resolution(4, 4).build().unwrap();
    let storage = destination.data().as_ptr();
    source.copy_into(&mut destination);
    assert_eq!(destination.data().as_ptr(), storage);
    assert_eq!(destination.data(), owned.data());
    assert_eq!(destination.timecode(), source.timecode());
    assert_eq!(destination.metadata(), source.metadata());
}

#[test]
fn borrowed_video_can_be_inspected_generically_and_erased() {
    let pixels = [
        1, 1, 1, 1, 1, 1, 1, 1, 99, 99, 99, 99, 2, 2, 2, 2, 2, 2, 2, 2, 99, 99, 99, 99,
    ];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .line_stride_bytes(12)
        .frame_rate(FrameRate::new(25, 1).unwrap())
        .timecode(Timecode::from_ticks(456))
        .metadata(c"<video/>")
        .build()
        .unwrap();
    inspect_video(&frame);
    inspect_video(&frame as &dyn VideoSource);
}

#[test]
fn borrowed_audio_rejects_invalid_layouts_and_metadata_before_native_access() {
    let data = [0.0; 16];
    for (rate, channels, samples) in [
        (0, 1, 1),
        (u32::MAX, 1, 1),
        (48_000, 0, 1),
        (48_000, 1, 0),
        (48_000, u32::MAX, u32::MAX),
    ] {
        assert!(BorrowedAudioFrame::builder(&data, rate, channels, samples)
            .build()
            .is_err());
    }
    for bytes in [0, 1, 4, 7, usize::MAX, i32::MAX as usize] {
        assert!(BorrowedAudioFrame::builder(&data, 48_000, 2, 2)
            .channel_stride_bytes(bytes)
            .build()
            .is_err());
    }
    assert!(BorrowedAudioFrame::builder(&data[..3], 48_000, 2, 2)
        .build()
        .is_err());
    let invalid = c"\xff";
    assert!(BorrowedAudioFrame::builder(&data, 48_000, 2, 2)
        .metadata(invalid)
        .build()
        .is_err());
    let packed = BorrowedAudioFrame::builder(&data, 48_000, 2, 2)
        .build()
        .unwrap();
    assert_eq!(packed.data(), Some(&data[..4]));
    let mono = BorrowedAudioFrame::builder(&data, 48_000, 1, 2)
        .channel_stride_bytes(64)
        .build()
        .unwrap();
    assert_eq!(mono.data(), Some(&data[..2]));
    let reset = BorrowedAudioFrame::builder(&data, 48_000, 2, 2)
        .channel_stride_bytes(64)
        .metadata(c"<override/>")
        .clear_channel_stride_bytes()
        .clear_metadata()
        .build()
        .unwrap();
    assert_eq!(reset.channel_stride_bytes(), 8);
    assert_eq!(reset.metadata(), None);
    assert_eq!(reset.data(), Some(&data[..4]));
}
