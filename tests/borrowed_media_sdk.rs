//! Synchronous forwarding and conversion of receiver and FrameSync captures.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    audio_conversion::{AudioConverter, AudioReferenceLevel},
    AudioFrame, AudioSource, FrameRate, FrameSync, FrameSyncAudioRequest, Ndi, Receiver,
    ReceiverColorFormat, ReceiverOptions, ScanType, Sender, SenderOptions, Timecode, VideoFrame,
    VideoSource,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

struct Stop<'a>(&'a AtomicBool);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn sender(ndi: &Ndi, name: &str) -> Sender {
    Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(name))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap()
}

fn receiver(ndi: &Ndi, sender: &Sender) -> Receiver {
    Receiver::new(
        ndi,
        &ReceiverOptions::new(common::loopback_source(ndi, sender).unwrap())
            .color_format(ReceiverColorFormat::BgrxBgra)
            .allow_video_fields(false),
    )
    .unwrap()
}

fn video(sender: &Sender, receiver: &Receiver, source: &impl VideoSource, expected: &VideoFrame) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        sender.send_video(source).unwrap();
        if let Some(frame) = receiver
            .video()
            .capture_ref(Duration::from_millis(100))
            .unwrap()
        {
            assert_eq!(
                (frame.width(), frame.height()),
                (expected.width(), expected.height())
            );
            assert_eq!(frame.frame_rate(), expected.frame_rate());
            assert_eq!(
                frame.picture_aspect_ratio(),
                expected.picture_aspect_ratio()
            );
            assert_eq!(frame.scan_type(), expected.scan_type());
            assert_eq!(frame.timecode(), expected.timecode());
            assert_eq!(frame.metadata(), expected.metadata());
            let copy = frame.to_owned();
            assert_eq!(copy.data().len(), expected.data().len());
            // NDI's video compression is lossy, including between identical
            // submissions. Compare active content with a small codec tolerance.
            let largest = copy
                .data()
                .iter()
                .zip(expected.data())
                .enumerate()
                .max_by_key(|(_, (a, b))| a.abs_diff(**b))
                .unwrap();
            assert!(
                copy.data()
                    .iter()
                    .zip(expected.data())
                    .all(|(a, b)| a.abs_diff(*b) <= 10),
                "format {:?}->{:?}, largest delta at {:?}, first actual {:?}, expected {:?}",
                expected.pixel_format(),
                frame.pixel_format(),
                largest,
                &copy.data()[..24],
                &expected.data()[..24]
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "borrowed video did not reach relay output"
        );
    }
}

fn audio(
    sender: &Sender,
    receiver: &Receiver,
    converter: &AudioConverter,
    source: &impl AudioSource,
    expected: &AudioFrame,
) {
    let len = expected.num_samples() as usize * expected.num_channels() as usize;
    let mut actual_f32 = vec![0.0; len + 2];
    let mut expected_f32 = vec![0.0; len + 2];
    converter
        .to_interleaved_f32(source, &mut actual_f32)
        .unwrap();
    converter
        .to_interleaved_f32(expected, &mut expected_f32)
        .unwrap();
    assert_eq!(actual_f32, expected_f32);
    let mut actual_i16 = vec![0; len + 2];
    let mut expected_i16 = vec![0; len + 2];
    converter
        .to_interleaved_i16(
            source,
            &mut actual_i16,
            AudioReferenceLevel::RECEIVE_HEADROOM,
        )
        .unwrap();
    converter
        .to_interleaved_i16(
            expected,
            &mut expected_i16,
            AudioReferenceLevel::RECEIVE_HEADROOM,
        )
        .unwrap();
    assert_eq!(actual_i16, expected_i16);
    let mut actual_i32 = vec![0; len + 2];
    let mut expected_i32 = vec![0; len + 2];
    converter
        .to_interleaved_i32(
            source,
            &mut actual_i32,
            AudioReferenceLevel::RECEIVE_HEADROOM,
        )
        .unwrap();
    converter
        .to_interleaved_i32(
            expected,
            &mut expected_i32,
            AudioReferenceLevel::RECEIVE_HEADROOM,
        )
        .unwrap();
    assert_eq!(actual_i32, expected_i32);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        sender.send_audio(source);
        if let Some(frame) = receiver
            .audio()
            .capture_ref(Duration::from_millis(100))
            .unwrap()
        {
            assert_eq!(frame.sample_rate(), expected.sample_rate());
            assert_eq!(frame.num_channels(), expected.num_channels());
            assert_eq!(frame.timecode(), expected.timecode());
            assert_eq!(frame.metadata(), expected.metadata());
            assert_eq!(frame.num_samples(), expected.num_samples());
            for (channel, expected) in frame.channels().zip(expected.channels()) {
                assert!(channel
                    .iter()
                    .zip(expected)
                    .all(|(value, expected)| (*value - *expected).abs() < 0.0001));
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "borrowed audio did not reach relay output"
        );
    }
}

#[test]
fn receiver_and_framesync_captures_forward_and_convert_without_owned_adapters() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let source = sender(&ndi, "borrowed media source");
    let input = receiver(&ndi, &source);
    let relay = sender(&ndi, "borrowed media relay");
    let output = receiver(&ndi, &relay);
    // A distinct relay/output pair prevents earlier raw captures from satisfying
    // the later FrameSync phase through queued network frames.
    let synced_relay = sender(&ndi, "borrowed FrameSync relay");
    let synced_output = receiver(&ndi, &synced_relay);
    let converter = AudioConverter::new(&ndi).unwrap();
    let mut pixels = VideoFrame::builder()
        .resolution(66, 48)
        .frame_rate(FrameRate::new(30_000, 1_001).unwrap())
        .picture_aspect_ratio(1.25)
        .timecode(Timecode::from_ticks(123_456))
        .metadata("<relay-video/>")
        .build()
        .unwrap();
    for pixel in pixels.data_mut().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[32, 96, 160, 255]);
    }
    let mut samples = AudioFrame::builder()
        .num_channels(2)
        .num_samples(480)
        .timecode(Timecode::from_ticks(456_789))
        .metadata("<relay-audio/>")
        .build()
        .unwrap();
    samples.data_mut()[..480].fill(0.25);
    samples.data_mut()[480..].fill(-0.5);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                source.send_video(&pixels).unwrap();
                source.send_audio(&samples);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let _stop = Stop(&stop);
        let frame = input
            .video()
            .capture_ref(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        video(&relay, &output, &frame, &frame.to_owned());
        drop(frame);
        let frame = input
            .audio()
            .capture_ref(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        audio(&relay, &output, &converter, &frame, &frame.to_owned());
        drop(frame);
        let framesync = FrameSync::new(input).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(frame) = framesync.capture_video_ref(ScanType::Progressive).unwrap() {
                video(&synced_relay, &synced_output, &frame, &frame.to_owned());
                break;
            }
            assert!(Instant::now() < deadline, "FrameSync video unavailable");
            std::thread::sleep(Duration::from_millis(10));
        }
        let request = FrameSyncAudioRequest::capture_at(48_000, 2, 480).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let frame = framesync.capture_audio_ref(request).unwrap();
            if frame
                .channels()
                .any(|channel| channel.iter().any(|sample| sample.abs() > 0.1))
            {
                audio(
                    &synced_relay,
                    &synced_output,
                    &converter,
                    &frame,
                    &frame.to_owned(),
                );
                break;
            }
            assert!(Instant::now() < deadline, "FrameSync audio remained silent");
            std::thread::sleep(Duration::from_millis(10));
        }
    });
}
