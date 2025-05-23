// This test verifies that buffers cannot be reused while an async token is held

use grafton_ndi::{NDI, SendOptions, VideoFrameBorrowed, AudioFrameBorrowed, MetadataFrameBorrowed};
use std::ffi::CString;

fn main() {
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("Test")
        .clock_video(true)
        .clock_audio(true)
        .build()
        .unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    // Test 1: Video buffer cannot be reused while token is held
    {
        let mut video_buffer = vec![0u8; 1920 * 1080 * 4];
        let frame = VideoFrameBorrowed::from_buffer(&video_buffer, 1920, 1080, 
            grafton_ndi::FourCCVideoType::BGRA, 30, 1);
        
        let _token = send.send_video_async(&frame);
        
        // This should fail to compile - buffer is borrowed mutably by the token
        video_buffer[0] = 1; //~ ERROR cannot borrow `video_buffer` as mutable
    }

    // Test 2: Audio buffer cannot be reused while token is held
    {
        let mut audio_buffer = vec![0u8; 48000 * 2 * 4];
        let frame = AudioFrameBorrowed::from_buffer(&audio_buffer, 48000, 2, 48000);
        
        let _token = send.send_audio_async(&frame);
        
        // This should fail to compile - buffer is borrowed mutably by the token
        audio_buffer[0] = 1; //~ ERROR cannot borrow `audio_buffer` as mutable
    }

    // Test 3: Metadata buffer cannot be reused while token is held
    {
        let metadata = CString::new("<xml>test</xml>").unwrap();
        let frame = MetadataFrameBorrowed::new(&metadata);
        
        let _token = send.send_metadata_async(&frame);
        
        // This would fail if metadata was mutable - showing the token holds a reference
        // metadata.as_bytes_with_nul_mut()[0] = b'X'; // Would error if metadata was mut
    }

    // Test 4: Buffer can be reused after token is dropped
    {
        let mut video_buffer = vec![0u8; 1920 * 1080 * 4];
        
        {
            let frame = VideoFrameBorrowed::from_buffer(&video_buffer, 1920, 1080, 
                grafton_ndi::FourCCVideoType::BGRA, 30, 1);
            let _token = send.send_video_async(&frame);
            // Token is dropped here
        }
        
        // This should compile fine - token has been dropped
        video_buffer[0] = 1;
    }
}