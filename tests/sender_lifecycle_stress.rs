//! Owned async send lifecycle stress. The default path has no SDK callback.
use grafton_ndi::{Sender, SenderOptions, VideoFrame, NDI};

#[test]
#[ignore = "Requires an installed NDI runtime; exercises independent Windows flushes"]
fn independent_senders_flush_finish_and_drop_concurrently() {
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    let barrier = Arc::new(Barrier::new(4));
    let (done_tx, done_rx) = mpsc::channel();
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            let done_tx = done_tx.clone();
            std::thread::spawn(move || {
                let ndi = NDI::new().unwrap();
                let mut sender = Sender::new(
                    &ndi,
                    &SenderOptions::builder(format!("independent flush {index}"))
                        .clock_video(false)
                        .clock_audio(false)
                        .build(),
                )
                .unwrap();
                barrier.wait();
                for value in 0..20 {
                    let mut frame = VideoFrame::builder().resolution(16, 16).build().unwrap();
                    frame.data_mut().fill(value);
                    let allocation = frame.data().as_ptr();
                    sender.send_video_async(frame);
                    sender.flush_async_blocking();
                    let recovered = sender.finish_video_async().unwrap();
                    assert_eq!(recovered.data().as_ptr(), allocation);
                    assert!(recovered.data().iter().all(|&byte| byte == value));
                }
                sender.send_video_async(VideoFrame::builder().resolution(16, 16).build().unwrap());
                drop(sender); // Also drains an outstanding frame.
                done_tx.send(()).unwrap();
            })
        })
        .collect();
    for _ in 0..4 {
        // Detached workers keep a deadlocked SDK from hanging the test runner.
        done_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("independent sender stalled");
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
#[ignore = "Slow stress test requiring an installed NDI runtime"]
fn rapid_owned_sender_lifecycle() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    for iteration in 0..20 {
        let options = SenderOptions::builder(format!("owned lifecycle {iteration}"))
            .clock_video(false)
            .clock_audio(false)
            .build();
        let mut sender = Sender::new(&ndi, &options)?;
        let mut next = VideoFrame::builder().resolution(16, 16).build()?;
        for value in 0..50 {
            next.data_mut().fill(value);
            let returned = sender.send_video_async(next);
            next = match returned {
                Some(frame) => frame,
                None => VideoFrame::builder().resolution(16, 16).build()?,
            };
        }
        // Exercise sender-owned draining without an explicit finish call.
        drop(sender);
    }
    Ok(())
}
