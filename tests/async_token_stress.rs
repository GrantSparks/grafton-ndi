use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use grafton_ndi::{BorrowedVideoFrame, FourCCVideoType, SenderOptions, NDI};

#[test]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn stress_test_async_token_drops() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI - do this once before spawning threads to avoid race conditions
    let ndi = Arc::new(NDI::new()?);

    // Shared counter for completed operations
    let completed = Arc::new(Mutex::new(0));

    // Create multiple threads that send frames sequentially (single-flight API)
    let mut handles = vec![];

    for thread_id in 0..4 {
        let ndi_clone = ndi.clone();
        let completed_clone = completed.clone();
        let handle = thread::spawn(move || -> Result<(), grafton_ndi::Error> {
            let send_options = SenderOptions::builder(format!("Stress Test Sender {thread_id}"))
                .clock_video(true)
                .clock_audio(false)
                .build()?;
            let mut sender = grafton_ndi::Sender::new(&ndi_clone, &send_options)?;

            sender.on_async_video_done(move |_len| {
                let mut count = completed_clone.lock().unwrap();
                *count += 1;
            });

            let mut buffer = vec![0u8; 1920 * 1080 * 4];

            for frame_num in 0..250 {
                // Fill buffer with test pattern
                for (i, byte) in buffer.iter_mut().enumerate() {
                    *byte = ((i + frame_num + thread_id * 1000) % 256) as u8;
                }

                let borrowed_frame = BorrowedVideoFrame::from_buffer(
                    &buffer,
                    1920,
                    1080,
                    FourCCVideoType::BGRA,
                    30,
                    1,
                );

                // With the new API, we send one frame at a time
                let token = sender.send_video_async(&borrowed_frame);

                // Randomly drop the token early or hold it
                if (frame_num + thread_id) % 3 == 0 {
                    // Drop token immediately - this tests flush behavior
                    drop(token);
                } else {
                    // Hold token for a bit
                    thread::sleep(Duration::from_micros(100));
                    drop(token);
                }

                // Occasionally yield
                if frame_num % 10 == 0 {
                    thread::yield_now();
                }
            }
            Ok(())
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap()?;
    }

    // Wait a bit for all callbacks to complete
    thread::sleep(Duration::from_millis(100));

    let final_count = *completed.lock().unwrap();
    println!("Completed {final_count} async operations");

    // We sent 4 threads * 250 frames = 1000 frames
    assert_eq!(final_count, 1000, "Not all async callbacks were called");

    Ok(())
}

#[test]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_immediate_sender_drop() -> Result<(), grafton_ndi::Error> {
    // This test verifies that the single-flight API prevents UB
    let ndi = NDI::new()?;

    for _ in 0..100 {
        let send_options = SenderOptions::builder("Immediate Drop Test")
            .clock_video(true)
            .clock_audio(false)
            .build()?;
        let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

        // Create a scope to control lifetimes
        {
            let buffer = vec![0u8; 1920 * 1080 * 4];
            let borrowed_frame =
                BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);

            // Send async - the token holds borrows ensuring safety
            let _token = sender.send_video_async(&borrowed_frame);

            // The token and buffer must be dropped before sender
            // The new API prevents the buffer from being freed while the token exists
        }

        sender.flush_async_blocking();

        // Now drop sender - this is safe because tokens enforce proper ordering
        drop(sender);
    }

    Ok(())
}

#[test]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_flush_async() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let send_options = SenderOptions::builder("Flush Test")
        .clock_video(true)
        .clock_audio(false)
        .build()?;
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    // With single-flight API, we send frames sequentially
    let buffers: Vec<Vec<u8>> = (0..10).map(|i| vec![i as u8; 1920 * 1080 * 4]).collect();

    // Send each frame sequentially
    for buffer in buffers.iter() {
        let borrowed_frame =
            BorrowedVideoFrame::from_buffer(buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
        let token = sender.send_video_async(&borrowed_frame);
        // Token automatically flushes on drop
        drop(token);
    }

    sender.flush_async_blocking();

    Ok(())
}
