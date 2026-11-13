//! Move owned frames into NDI and reuse two pixel allocations without per-send wrapper copies.
//! SDK encoding and network processing may still copy data.
use grafton_ndi::{FrameRate, Ndi, PixelFormat, SenderOptions, VideoFrame};

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = Ndi::new()?;
    let options = SenderOptions::new("Zero Copy Async Sender").clock_video(true);
    let mut sender = grafton_ndi::Sender::new(&ndi, &options)?;
    let make_frame = || -> grafton_ndi::Result<VideoFrame> {
        Ok(VideoFrame::builder()
            .resolution(1920, 1080)
            .pixel_format(PixelFormat::Bgra)
            .frame_rate(FrameRate::new(60, 1)?)
            .build()?)
    };
    let first = make_frame()?;
    let mut available = make_frame()?;
    assert!(sender.send_video_async(first)?.is_none());

    for frame_num in 1..300 {
        generate_test_pattern(available.data_mut(), 1920, 1080, frame_num);
        // The next submission completes the previous send before returning its frame.
        // Moving VideoFrame transfers its allocation; it does not copy its pixels.
        available = sender.send_video_async(available)?.expect("previous frame");
    }
    let final_frame = sender.finish_video_async().expect("final pending frame");
    println!(
        "Recovered both allocations: {} and {} bytes",
        available.data().len(),
        final_frame.data().len()
    );
    Ok(())
}

/// Generate a simple test pattern with moving gradients
///
/// # Arguments
/// * `buffer` - The buffer to fill with BGRA pixel data
/// * `width` - Frame width in pixels
/// * `height` - Frame height in pixels
/// * `frame_num` - Current frame number for animation
fn generate_test_pattern(buffer: &mut [u8], width: u32, height: u32, frame_num: u32) {
    let width = width as usize;
    let height = height as usize;

    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;

            // Create a moving gradient pattern
            let r = ((x + frame_num as usize) % 256) as u8;
            let g = ((y + frame_num as usize) % 256) as u8;
            let b = ((x + y + frame_num as usize) % 256) as u8;

            buffer[offset] = b; // B
            buffer[offset + 1] = g; // G
            buffer[offset + 2] = r; // R
            buffer[offset + 3] = 255; // A
        }
    }
}
