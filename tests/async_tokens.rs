use grafton_ndi::{
    AudioFrameBorrowed, FourCCVideoType, MetadataFrameBorrowed, SendOptions, VideoFrameBorrowed,
    NDI,
};
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[test]
fn test_video_async_token_lifetime() {
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("TestVideo")
        .clock_video(true)
        .build()
        .unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    // Track when callback is called
    let callback_called = Arc::new(Mutex::new(false));
    let callback_called_clone = callback_called.clone();

    send.on_async_video_done(move |buf| {
        // Verify we can access the buffer
        assert!(!buf.is_empty());
        *callback_called_clone.lock().unwrap() = true;
    });

    let mut buffer = vec![42u8; 1920 * 1080 * 4];

    {
        let frame =
            VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
        let _token = send.send_video_async(&frame);

        // Token still held, callback should not be called yet
        thread::sleep(Duration::from_millis(10));
        assert!(!*callback_called.lock().unwrap());
    }

    // Token dropped, callback should be called
    thread::sleep(Duration::from_millis(10));
    assert!(*callback_called.lock().unwrap());

    // Buffer should be safe to modify now
    buffer[0] = 0;
}

#[test]
fn test_audio_async_token_lifetime() {
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("TestAudio")
        .clock_audio(true)
        .build()
        .unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    // Track when callback is called
    let callback_called = Arc::new(Mutex::new(false));
    let callback_called_clone = callback_called.clone();

    send.on_async_audio_done(move |buf| {
        // Verify we can access the buffer
        assert!(!buf.is_empty());
        *callback_called_clone.lock().unwrap() = true;
    });

    let mut buffer = vec![0u8; 48000 * 2 * 4]; // 1 second stereo float32

    {
        let frame = AudioFrameBorrowed::from_buffer(&buffer, 48000, 2, 48000);
        let _token = send.send_audio_async(&frame);

        // Token still held, callback should not be called yet
        thread::sleep(Duration::from_millis(10));
        assert!(!*callback_called.lock().unwrap());
    }

    // Token dropped, callback should be called
    thread::sleep(Duration::from_millis(10));
    assert!(*callback_called.lock().unwrap());

    // Buffer should be safe to modify now
    buffer[0] = 42;
}

#[test]
fn test_metadata_async_token_lifetime() {
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("TestMetadata").build().unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    // Track when callback is called
    let callback_called = Arc::new(Mutex::new(false));
    let callback_called_clone = callback_called.clone();

    send.on_async_metadata_done(move |buf| {
        // Verify we can access the buffer
        assert!(!buf.is_empty());
        *callback_called_clone.lock().unwrap() = true;
    });

    let metadata = CString::new("<xml>test data</xml>").unwrap();

    {
        let frame = MetadataFrameBorrowed::new(&metadata);
        let _token = send.send_metadata_async(&frame);

        // Token still held, callback should not be called yet
        thread::sleep(Duration::from_millis(10));
        assert!(!*callback_called.lock().unwrap());
    }

    // Token dropped, callback should be called
    thread::sleep(Duration::from_millis(10));
    assert!(*callback_called.lock().unwrap());
}

#[test]
fn test_multiple_async_sends() {
    // Test sending multiple frame types sequentially
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("TestMultiple")
        .clock_video(true)
        .clock_audio(true)
        .build()
        .unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    // Send video
    {
        let buffer = vec![0u8; 1920 * 1080 * 4];
        let frame =
            VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
        let _token = send.send_video_async(&frame);
        thread::sleep(Duration::from_millis(10));
    }

    // Send audio
    {
        let buffer = vec![0u8; 48000 * 2 * 4];
        let frame = AudioFrameBorrowed::from_buffer(&buffer, 48000, 2, 48000);
        let _token = send.send_audio_async(&frame);
        thread::sleep(Duration::from_millis(10));
    }

    // Send metadata
    {
        let metadata = CString::new("<xml>sequential test</xml>").unwrap();
        let frame = MetadataFrameBorrowed::new(&metadata);
        let _token = send.send_metadata_async(&frame);
        thread::sleep(Duration::from_millis(10));
    }
}
