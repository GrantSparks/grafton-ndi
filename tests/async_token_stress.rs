use grafton_ndi::{BorrowedVideoFrame, PixelFormat, SenderOptions, NDI};

use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[test]
#[ignore = "Slow stress test - run with --ignored"]
fn stress_test_async_token_drops() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI once before spawning threads to avoid race conditions
    let ndi = Arc::new(NDI::new()?);

    let completed = Arc::new(Mutex::new(0));
    let mut handles = vec![];

    for thread_id in 0..4 {
        let ndi_clone = ndi.clone();
        let completed_clone = completed.clone();
        let handle = thread::spawn(move || -> Result<(), grafton_ndi::Error> {
            let send_options = SenderOptions::builder(format!("Stress Test Sender {thread_id}"))
                .clock_video(true)
                .clock_audio(false)
                .build();
            let mut sender = grafton_ndi::Sender::new(&ndi_clone, &send_options)?;

            sender.on_async_video_done(move |_len| {
                let mut count = completed_clone.lock().unwrap();
                *count += 1;
            });

            let mut buffer = vec![0u8; 1920 * 1080 * 4];

            for frame_num in 0..250 {
                for (i, byte) in buffer.iter_mut().enumerate() {
                    *byte = ((i + frame_num + thread_id * 1000) % 256) as u8;
                }

                let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(
                    &buffer,
                    1920,
                    1080,
                    PixelFormat::BGRA,
                    30,
                    1,
                )?;

                let token = sender.send_video_async(&borrowed_frame);

                if (frame_num + thread_id) % 3 == 0 {
                    drop(token);
                } else {
                    thread::sleep(Duration::from_micros(100));
                    drop(token);
                }

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

    thread::sleep(Duration::from_millis(100));

    let final_count = *completed.lock().unwrap();
    println!("Completed {final_count} async operations");

    assert_eq!(final_count, 1000, "Not all async callbacks were called");

    Ok(())
}

#[test]
#[ignore = "Slow stress test - run with --ignored"]
fn test_immediate_sender_drop() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;

    for _ in 0..100 {
        let send_options = SenderOptions::builder("Immediate Drop Test")
            .clock_video(true)
            .clock_audio(false)
            .build();
        let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

        {
            let buffer = vec![0u8; 1920 * 1080 * 4];
            let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(
                &buffer,
                1920,
                1080,
                PixelFormat::BGRA,
                30,
                1,
            )?;

            let _token = sender.send_video_async(&borrowed_frame);
        }

        sender.flush_async_blocking();
        drop(sender);
    }

    Ok(())
}

#[test]
#[ignore = "Slow stress test - run with --ignored"]
fn test_flush_async() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let send_options = SenderOptions::builder("Flush Test")
        .clock_video(true)
        .clock_audio(false)
        .build();
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    let buffers: Vec<Vec<u8>> = (0..10).map(|i| vec![i as u8; 1920 * 1080 * 4]).collect();

    for buffer in buffers.iter() {
        let borrowed_frame = BorrowedVideoFrame::try_from_uncompressed(
            buffer,
            1920,
            1080,
            PixelFormat::BGRA,
            30,
            1,
        )?;
        let token = sender.send_video_async(&borrowed_frame);
        drop(token);
    }

    sender.flush_async_blocking();

    Ok(())
}
