use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

    // Create multiple threads that send frames and drop tokens at random times
    let mut handles = vec![];

    for thread_id in 0..4 {
        let ndi_clone = ndi.clone();
        let completed_clone = completed.clone();
        let handle = thread::spawn(move || -> Result<(), grafton_ndi::Error> {
            let send_options = SenderOptions::builder(format!("Stress Test Sender {thread_id}"))
                .clock_video(true)
                .clock_audio(false)
                .build()?;
            let sender = grafton_ndi::Sender::new(&ndi_clone, &send_options)?;

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

                let token = sender.send_video_async(&borrowed_frame);

                // Randomly drop the token early or hold it
                if (frame_num + thread_id) % 3 == 0 {
                    // Drop token immediately - this tests the race condition
                    drop(token);
                } else {
                    // Hold token for a bit
                    thread::sleep(Duration::from_micros(100));
                    drop(token);
                }

                // Occasionally yield to increase chance of race conditions
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
    // This test specifically targets the original bug
    let ndi = NDI::new()?;

    for _ in 0..100 {
        let send_options = SenderOptions::builder("Immediate Drop Test")
            .clock_video(true)
            .clock_audio(false)
            .build()?;
        let sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

        // Create a scope to control lifetimes
        {
            let buffer = vec![0u8; 1920 * 1080 * 4];
            let borrowed_frame =
                BorrowedVideoFrame::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);

            // Send async - the token now holds Arc<Inner>
            let _token = sender.send_video_async(&borrowed_frame);

            // The token and buffer must be dropped before sender
            // This simulates the original race condition
        }

        sender.flush_async_blocking();

        // Now drop sender - this will block until the token is dropped
        // The fix ensures this is safe by using Arc<Inner>
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
    let sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    // Use a scope to control buffer lifetimes
    {
        let mut buffers = vec![];
        let mut tokens = vec![];

        for i in 0..10 {
            buffers.push(vec![i as u8; 1920 * 1080 * 4]);
        }

        // Then create tokens that borrow from buffers
        for buffer in buffers.iter() {
            let borrowed_frame =
                BorrowedVideoFrame::from_buffer(buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
            tokens.push(sender.send_video_async(&borrowed_frame));
        }

        // Drop tokens before buffers go out of scope
        drop(tokens);
    }

    sender.flush_async_blocking();

    Ok(())
}
