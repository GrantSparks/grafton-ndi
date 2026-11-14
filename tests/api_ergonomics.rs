//! Cross-family contracts that should remain predictable as frame types evolve.
use grafton_ndi::{AudioFrame, BorrowedVideoFrame, FrameRate, PixelFormat, Timecode, VideoFrame};

#[test]
fn video_construction_uses_the_same_defaults_and_preserves_explicit_timing() {
    let owned = VideoFrame::builder().resolution(4, 2).build().unwrap();
    let pixels = [0; 32];
    let borrowed = BorrowedVideoFrame::builder(&pixels, 4, 2, PixelFormat::Bgra)
        .build()
        .unwrap();
    assert_eq!(owned.frame_rate(), FrameRate::new(60, 1).unwrap());
    assert_eq!(borrowed.frame_rate(), owned.frame_rate());
    assert_eq!(
        borrowed.picture_aspect_ratio(),
        owned.picture_aspect_ratio()
    );
    assert_eq!(borrowed.scan_type(), owned.scan_type());
    assert_eq!(borrowed.timecode(), owned.timecode());
    assert_eq!(borrowed.metadata(), owned.metadata());

    let rate = FrameRate::new(30_000, 1001).unwrap();
    let borrowed = BorrowedVideoFrame::builder(&pixels, 4, 2, PixelFormat::Bgra)
        .frame_rate(rate)
        .build()
        .unwrap();
    assert_eq!(borrowed.to_owned().frame_rate(), rate);

    #[cfg(feature = "advanced")]
    {
        use grafton_ndi::advanced::{VideoCodec, VideoPacketFormat};
        let format = VideoPacketFormat::new(VideoCodec::H264, 4, 2).unwrap();
        assert_eq!(format.frame_rate(), owned.frame_rate());
        assert_eq!(format.picture_aspect_ratio(), owned.picture_aspect_ratio());
        assert_eq!(format.scan_type(), owned.scan_type());
        assert_eq!(format.with_frame_rate(rate).frame_rate(), rate);
    }
}

// These assertions exercise the public methods on each owned storage family,
// including rejection without losing the previous value and absent vs empty text.
macro_rules! check_frame_editing {
    ($frame:expr) => {{
        let mut frame = $frame;
        assert_eq!(frame.metadata(), None);
        frame.set_timecode(Timecode::from_ticks(123));
        frame.set_metadata("<original/>").unwrap();
        let snapshot = frame.clone();
        for invalid in ["bad\0text".to_owned(), "x".repeat(4 * 1024 * 1024)] {
            assert!(frame.set_metadata(invalid).is_err());
            assert_eq!(frame.metadata(), Some("<original/>"));
            assert_eq!(frame.timecode(), Timecode::from_ticks(123));
        }
        frame.set_metadata("<replacement/>").unwrap();
        assert_eq!(frame.metadata(), Some("<replacement/>"));
        assert_eq!(snapshot.metadata(), Some("<original/>"));
        frame.clear_metadata();
        frame.clear_metadata();
        assert_eq!(frame.metadata(), None);
        frame.set_metadata("").unwrap();
        assert_eq!(frame.metadata(), Some(""));
        assert_eq!(frame.timecode(), Timecode::from_ticks(123));
    }};
}

#[test]
fn raw_frames_share_metadata_and_timecode_editing() {
    check_frame_editing!(VideoFrame::builder().resolution(4, 2).build().unwrap());
    check_frame_editing!(AudioFrame::builder().num_samples(2).build().unwrap());
}

#[cfg(feature = "advanced")]
#[test]
fn compressed_and_scatter_frames_share_owned_frame_editing() {
    use grafton_ndi::advanced::{
        AudioCodec, CompressedAudioFrame, CompressedVideoFrame, ScatterAudioFrame,
        ScatterVideoFrame, VideoCodec, VideoPacketFormat,
    };
    let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16).unwrap();
    check_frame_editing!(CompressedVideoFrame::new(format, vec![0; 4]).unwrap());
    check_frame_editing!(ScatterVideoFrame::new(format, vec![vec![0; 4]]).unwrap());
    check_frame_editing!(
        CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![1]).unwrap()
    );
    check_frame_editing!(
        ScatterAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![vec![1]]).unwrap()
    );
}
