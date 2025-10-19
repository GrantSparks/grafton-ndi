/// Advanced SDK callback lifetime stress tests
///
/// These tests verify that the memory-safe callback implementation in issue #18
/// correctly handles rapid create/send/drop cycles without UB or leaks.
///
/// These tests are ignored by default because they take ~100 seconds to run.
/// Run them explicitly with: cargo test --features advanced_sdk --test callback_lifetime_stress -- --ignored

#[cfg(feature = "advanced_sdk")]
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

#[cfg(feature = "advanced_sdk")]
use grafton_ndi::{BorrowedVideoFrame, FourCCVideoType, SenderOptions, NDI};

/// Test rapid create → send → drop loop to verify callback unregistration
///
/// This ensures:
/// - No UB from callback accessing freed Inner
/// - NDIlib_send_destroy is reached (verified by no panics/crashes)
/// - Callback unregistration happens correctly
#[test]
#[ignore = "Slow stress test - run with --ignored"]
#[cfg(feature = "advanced_sdk")]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_rapid_sender_lifecycle() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let callback_count = Arc::new(AtomicUsize::new(0));

    // Rapid loop: create sender, send frames, drop sender
    for iteration in 0..10 {
        let send_options = SenderOptions::builder(format!("Lifecycle Test {iteration}"))
            .clock_video(true)
            .clock_audio(false)
            .build()?;
        let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

        // Register callback to track completions
        let counter = callback_count.clone();
        sender.on_async_video_done(move |_len| {
            counter.fetch_add(1, Ordering::Relaxed);
        });

        // Send a few frames rapidly
        let buffer = vec![0u8; 640 * 480 * 4];
        for _ in 0..2 {
            let borrowed_frame =
                BorrowedVideoFrame::from_buffer(&buffer, 640, 480, FourCCVideoType::BGRA, 30, 1);
            let _token = sender.send_video_async(&borrowed_frame);
            // Drop token immediately to stress the callback path
        }

        // Sender drop should:
        // 1. Unregister callback
        // 2. Wait for in-flight callbacks
        // 3. Destroy NDI sender instance
        drop(sender);
    }

    // Allow time for any stragglers
    thread::sleep(Duration::from_millis(100));

    let final_count = callback_count.load(Ordering::Relaxed);
    println!("Rapid lifecycle test: {final_count} callbacks completed");

    // We should see some callbacks (exact count may vary due to NDI timing)
    // The key is no crashes or UB
    assert!(
        final_count > 0,
        "Expected at least some callbacks to complete"
    );

    Ok(())
}

/// Test concurrent create/send/drop from multiple threads
///
/// This verifies:
/// - Thread-safe callback registration/unregistration
/// - No data races in callback pointer handling
/// - Correct cleanup under concurrent load
#[test]
#[ignore = "Slow stress test - run with --ignored"]
#[cfg(feature = "advanced_sdk")]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_concurrent_sender_lifecycle() -> Result<(), grafton_ndi::Error> {
    let ndi = Arc::new(NDI::new()?);
    let total_callbacks = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn threads that each create/use/drop their own sender
    for thread_id in 0..2 {
        let ndi_clone = ndi.clone();
        let callback_count = total_callbacks.clone();

        let handle = thread::spawn(move || -> Result<(), grafton_ndi::Error> {
            for iteration in 0..5 {
                let send_options =
                    SenderOptions::builder(format!("Thread {thread_id} Iter {iteration}"))
                        .clock_video(true)
                        .clock_audio(false)
                        .build()?;
                let mut sender = grafton_ndi::Sender::new(&ndi_clone, &send_options)?;

                let counter = callback_count.clone();
                sender.on_async_video_done(move |_len| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });

                // Send frames
                let buffer = vec![0u8; 640 * 480 * 4];
                for _ in 0..2 {
                    let borrowed_frame = BorrowedVideoFrame::from_buffer(
                        &buffer,
                        640,
                        480,
                        FourCCVideoType::BGRA,
                        30,
                        1,
                    );
                    let _token = sender.send_video_async(&borrowed_frame);
                }

                // Drop sender (should safely unregister callback)
                drop(sender);

                // Small sleep to interleave operations
                thread::sleep(Duration::from_micros(100));
            }
            Ok(())
        });

        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap()?;
    }

    // Allow callbacks to complete
    thread::sleep(Duration::from_millis(100));

    let final_count = total_callbacks.load(Ordering::Relaxed);
    println!("Concurrent lifecycle test: {final_count} callbacks completed");

    // Key assertion: no crashes, no UB
    assert!(
        final_count > 0,
        "Expected at least some callbacks to complete"
    );

    Ok(())
}

/// Test that callback doesn't execute after sender drop completes
///
/// This verifies the bounded wait in Sender::drop ensures no post-drop callbacks
#[test]
#[ignore = "Slow stress test - run with --ignored"]
#[cfg(feature = "advanced_sdk")]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_no_callbacks_after_drop() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let callback_count = Arc::new(AtomicUsize::new(0));

    {
        let send_options = SenderOptions::builder("Post-Drop Test")
            .clock_video(true)
            .clock_audio(false)
            .build()?;
        let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

        let counter = callback_count.clone();
        sender.on_async_video_done(move |_len| {
            counter.fetch_add(1, Ordering::Relaxed);
        });

        // Send frames
        let buffer = vec![0u8; 640 * 480 * 4];
        for _ in 0..3 {
            let borrowed_frame =
                BorrowedVideoFrame::from_buffer(&buffer, 640, 480, FourCCVideoType::BGRA, 30, 1);
            let _token = sender.send_video_async(&borrowed_frame);
        }

        // Explicit drop with bounded wait
        drop(sender);

        // After drop completes, no more callbacks should fire
    }

    let count_after_drop = callback_count.load(Ordering::Relaxed);

    // Wait a bit longer
    thread::sleep(Duration::from_millis(200));

    let count_after_wait = callback_count.load(Ordering::Relaxed);

    // Callback count should not increase after sender is dropped
    assert_eq!(
        count_after_drop, count_after_wait,
        "Callbacks should not execute after sender drop completes"
    );

    println!("No post-drop callbacks test: {count_after_wait} callbacks (no increase after drop)");

    Ok(())
}

/// Test that flush_async_blocking waits for callbacks
#[test]
#[ignore = "Slow stress test - run with --ignored"]
#[cfg(feature = "advanced_sdk")]
#[cfg_attr(
    all(target_os = "windows", target_env = "msvc"),
    ignore = "Skipping on Windows CI due to NDI runtime issues"
)]
fn test_flush_waits_for_callback() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let callback_count = Arc::new(AtomicUsize::new(0));

    let send_options = SenderOptions::builder("Flush Wait Test")
        .clock_video(true)
        .clock_audio(false)
        .build()?;
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    let counter = callback_count.clone();
    sender.on_async_video_done(move |_len| {
        counter.fetch_add(1, Ordering::Relaxed);
    });

    // Send frames
    let buffer = vec![0u8; 640 * 480 * 4];
    for _ in 0..3 {
        let borrowed_frame =
            BorrowedVideoFrame::from_buffer(&buffer, 640, 480, FourCCVideoType::BGRA, 30, 1);
        let _token = sender.send_video_async(&borrowed_frame);
    }

    // Flush should wait for all callbacks
    sender.flush_async_blocking();

    let count_after_flush = callback_count.load(Ordering::Relaxed);

    println!("Flush wait test: {count_after_flush} callbacks completed after flush");

    // After flush, some callbacks should have completed
    // (exact count depends on NDI timing, but should be > 0)
    assert!(
        count_after_flush > 0,
        "Expected callbacks to complete during flush"
    );

    Ok(())
}
