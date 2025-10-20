// This test verifies that video buffers cannot be reused while an async token is held

use grafton_ndi::{BorrowedVideoFrame, SenderOptions, NDI};

fn main() {
    let ndi = NDI::new().unwrap();
    let send_options = SenderOptions::builder("Test").clock_video(true).build();
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options).unwrap();

    // Test 1: Video buffer cannot be reused while token is held
    {
        let mut video_buffer = vec![0u8; 1920 * 1080 * 4];
        let frame = BorrowedVideoFrame::try_from_uncompressed(
            &video_buffer,
            1920,
            1080,
            grafton_ndi::PixelFormat::BGRA,
            30,
            1,
        )
        .unwrap();

        let _token = sender.send_video_async(&frame);

        // This should fail to compile - buffer is borrowed mutably by the token
        video_buffer[0] = 1; //~ ERROR cannot borrow `video_buffer` as mutable
    }

    // Test 2: Buffer can be reused after token is dropped
    {
        let mut video_buffer = vec![0u8; 1920 * 1080 * 4];

        {
            let frame = BorrowedVideoFrame::try_from_uncompressed(
                &video_buffer,
                1920,
                1080,
                grafton_ndi::PixelFormat::BGRA,
                30,
                1,
            )
            .unwrap();
            let _token = sender.send_video_async(&frame);
            // Token is dropped here
        }

        // This should compile fine - token has been dropped
        video_buffer[0] = 1;
    }
}
