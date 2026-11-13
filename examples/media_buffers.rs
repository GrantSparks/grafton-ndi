//! Reuse owned buffers, borrow application audio/video, and inspect media sources.
use grafton_ndi::{
    video_conversion::{V210Layout, VideoConverter},
    AudioSource, BorrowedAudioFrame, BorrowedVideoFrame, Ndi, PixelFormat, Sender, SenderOptions,
    VideoFrame, VideoSource,
};

fn inspect_video(source: &dyn VideoSource) {
    println!(
        "Video: {}x{}, {:?}, {} active planes",
        source.width(),
        source.height(),
        source.pixel_format(),
        source.planes().len(),
    );
}

fn inspect_audio(source: &dyn AudioSource) {
    println!(
        "Audio: {} Hz, {} channels, {} samples per channel",
        source.sample_rate(),
        source.num_channels(),
        source.num_samples(),
    );
    for channel in source.channels() {
        assert_eq!(channel.len(), source.num_samples() as usize);
    }
}

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::new()?;
    let template = SenderOptions::new("Media buffers")
        .clock_video(false)
        .clock_audio(false);
    let sender = Sender::new(&ndi, &template.clone().name("Media buffer example"))?;

    // A failed build returns the builder with its original allocation.
    let pixels = vec![0u8; 16 * 2 * 4];
    let rejected = VideoFrame::builder()
        .resolution(0, 2)
        .data(pixels)
        .build()
        .unwrap_err();
    let mut frame = rejected.into_inner().resolution(16, 2).build()?;
    inspect_video(&frame);
    sender.send_video(&frame)?;

    // Recycle both allocations rather than discarding the replaced buffer.
    let reusable = frame.swap_data(vec![127; 16 * 2 * 4])?;
    sender.send_video(&frame)?;
    let second = frame.into_data();
    println!(
        "Recovered pixel buffers: {} and {} bytes",
        reusable.len(),
        second.len()
    );

    // Channels are planar, with one initialized padding sample between them.
    // A real audio callback can borrow its existing buffer in the same way.
    let samples = [0.0, 0.1, 0.2, 0.3, 99.0, 0.4, 0.5, 0.6, 0.7];
    let audio = BorrowedAudioFrame::builder(&samples, 48_000, 2, 4)
        .channel_stride_bytes(5 * size_of::<f32>())
        .build()?;
    inspect_audio(&audio);
    sender.send_audio(&audio);

    // Native conversion also borrows existing pixels; source row padding and
    // output row padding are described independently by their validated layouts.
    let p216 = [0u8; 32]; // Two 12-byte active rows, each with 4 padding bytes.
    let video = BorrowedVideoFrame::builder(&p216, 6, 1, PixelFormat::P216)
        .line_stride_bytes(16)
        .build()?;
    inspect_video(&video);
    let layout = V210Layout::new(6, 1)?;
    let mut v210 = vec![0u32; layout.required_words()];
    VideoConverter::new(&ndi)?.p216_to_v210(&video as &dyn VideoSource, layout, &mut v210)?;
    println!("Converted borrowed P216 into {} V210 words", v210.len());
    Ok(())
}
