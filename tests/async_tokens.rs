use grafton_ndi::{FourCCVideoType, SendOptions, VideoFrameBorrowed, NDI};
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

    send.on_async_video_done(move |len| {
        // Verify we got a valid buffer length
        assert!(len > 0);
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
fn test_multiple_async_sends() {
    let ndi = NDI::new().unwrap();
    let send_options = SendOptions::builder("TestMultiple")
        .clock_video(true)
        .build()
        .unwrap();
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options).unwrap();

    let callback_count = Arc::new(Mutex::new(0));
    let callback_count_clone = callback_count.clone();

    send.on_async_video_done(move |_| {
        *callback_count_clone.lock().unwrap() += 1;
    });

    // Send multiple frames
    for i in 0..3 {
        let buffer = vec![i as u8; 1920 * 1080 * 4];
        let frame =
            VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
        let _token = send.send_video_async(&frame);
        // Token drops immediately
    }

    // Wait for all callbacks
    thread::sleep(Duration::from_millis(50));
    assert_eq!(*callback_count.lock().unwrap(), 3);
}
