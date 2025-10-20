// This test verifies that metadata cannot be dropped while an async token is held
// It should fail to compile with a lifetime error

use std::ffi::CString;

use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, SenderOptions, NDI};

fn main() {
    let ndi = NDI::new().unwrap();
    let send_options = SenderOptions::builder("Test").clock_video(true).build();
    let mut sender = Sender::new(&ndi, &send_options).unwrap();

    // Test: Metadata cannot be dropped while token is held
    let mut video_buffer = vec![0u8; 1920 * 1080 * 4];

    let _token = {
        // Create metadata with a short lifetime
        let metadata_string = CString::new("test metadata").unwrap();

        // SAFETY: This is intentionally unsafe to test lifetime bounds
        // The buffer is properly sized, but metadata lifetime is intentionally incorrect for the test
        let frame = unsafe {
            use grafton_ndi::{LineStrideOrSize, ScanType};
            BorrowedVideoFrame::from_parts_unchecked(
                &video_buffer,
                1920,
                1080,
                PixelFormat::BGRA,
                30,
                1,
                16.0 / 9.0,
                ScanType::Progressive,
                0,
                LineStrideOrSize::LineStrideBytes(1920 * 4),
                Some(&metadata_string),
                0,
            )
        };

        sender.send_video_async(&frame)
        // metadata_string is dropped here
    };

    // This should fail to compile because _token holds a borrow to metadata
    // which was dropped when metadata_string went out of scope
    // Error: `metadata_string` does not live long enough
    drop(_token);
}
