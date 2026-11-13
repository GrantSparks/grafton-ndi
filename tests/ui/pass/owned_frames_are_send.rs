#![allow(unused)]

use grafton_ndi::{
    AudioFrame, FrameRate, MetadataFrame, PixelFormat, ScanType, Timecode, VideoFrame,
};

fn assert_send<T: Send>() {}
fn assert_clone<T: Clone>() {}

fn take_send<T: Send>(value: T) {
    drop(value);
}

fn main() -> Result<(), grafton_ndi::Error> {
    assert_send::<VideoFrame>();
    assert_send::<AudioFrame>();
    assert_send::<MetadataFrame>();
    assert_clone::<VideoFrame>();
    assert_clone::<AudioFrame>();
    assert_clone::<MetadataFrame>();

    let video = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::Bgra)
        .frame_rate(FrameRate::new(30, 1)?)
        .picture_aspect_ratio(1.0)
        .scan_type(ScanType::Progressive)
        .build()?;
    take_send(video.clone());
    take_send(video);

    let audio = AudioFrame::builder()
        .num_channels(2)
        .num_samples(16)
        .data(vec![0.0; 32])
        .build()?;
    take_send(audio);

    let metadata = MetadataFrame::new("<ndi/>")?.with_timecode(Timecode::from_ticks(0));
    take_send(metadata);

    Ok(())
}
