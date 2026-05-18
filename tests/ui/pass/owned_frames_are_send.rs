#![allow(unused)]

use grafton_ndi::{AudioFrame, MetadataFrame, PixelFormat, ScanType, VideoFrame};

fn assert_send<T: Send>() {}

fn take_send<T: Send>(value: T) {
    drop(value);
}

fn main() -> Result<(), grafton_ndi::Error> {
    assert_send::<VideoFrame>();
    assert_send::<AudioFrame>();
    assert_send::<MetadataFrame>();

    let video = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::BGRA)
        .frame_rate(30, 1)
        .aspect_ratio(1.0)
        .scan_type(ScanType::Progressive)
        .build()?;
    take_send(video);

    let audio = AudioFrame::builder()
        .channels(2)
        .samples(16)
        .data(vec![0.0; 32])
        .build()?;
    take_send(audio);

    let metadata = MetadataFrame::with_data("<ndi/>", 0)?;
    take_send(metadata);

    Ok(())
}
