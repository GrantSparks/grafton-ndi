#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use std::{
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};

use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, SenderOptions, VideoFrame};

fn frame(value: u8) -> VideoFrame {
    let mut frame = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::Bgra)
        .metadata(format!("<frame value=\"{value}\"/>"))
        .build()
        .unwrap();
    frame.data_mut().fill(value);
    frame
}

#[test]
fn owned_pipeline_returns_original_buffers_and_metadata() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let options = SenderOptions::new(common::unique_name("owned pipeline regression"))
        .clock_video(false)
        .clock_audio(false);
    let mut sender = Sender::new(&ndi, &options)?;
    let first = frame(1);
    let first_ptr = first.data().as_ptr();
    assert!(sender.submit_video(first)?.is_none());
    let second = frame(2);
    let second_ptr = second.data().as_ptr();
    let recovered = sender.submit_video(second)?.unwrap();
    assert_eq!(recovered.data().as_ptr(), first_ptr);
    assert!(recovered.data().iter().all(|byte| *byte == 1));
    assert_eq!(recovered.metadata(), Some("<frame value=\"1\"/>"));
    let recovered = sender.reclaim_video().unwrap();
    assert_eq!(recovered.data().as_ptr(), second_ptr);
    assert!(recovered.data().iter().all(|byte| *byte == 2));
    assert_eq!(recovered.metadata(), Some("<frame value=\"2\"/>"));
    assert!(sender.reclaim_video().is_none());
    Ok(())
}

#[test]
fn copied_send_survives_released_source() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let options = SenderOptions::new(common::unique_name("copied source release regression"))
        .clock_video(false)
        .clock_audio(false);
    let mut sender = Sender::new(&ndi, &options)?;
    let mut buffer = vec![7; 16 * 16 * 4];
    let borrowed = BorrowedVideoFrame::builder(&buffer, 16, 16, PixelFormat::Bgra).build()?;
    sender.submit_video_copy(&borrowed)?;
    buffer.fill(9);
    drop(buffer);
    let _ = sender.reclaim_video();
    Ok(())
}

#[test]
fn mixed_owned_and_copy_send_recovers_the_released_frames() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let options = SenderOptions::new(common::unique_name("mixed async regression"))
        .clock_video(false)
        .clock_audio(false);
    let mut sender = Sender::new(&ndi, &options)?;
    let owned = frame(3);
    let pixels = owned.data().as_ptr();
    sender.submit_video(owned)?;
    let source = vec![4; 16 * 16 * 4];
    let borrowed = BorrowedVideoFrame::builder(&source, 16, 16, PixelFormat::Bgra).build()?;
    sender.submit_video_copy(&borrowed)?;
    let recovered = sender.submit_video(frame(5))?.unwrap();
    assert_eq!(recovered.data()[0], 4);
    // A later copy reuses the original owned buffer, which only copying
    // retained; an owned submission still returns the frame it displaces.
    sender.submit_video_copy(&borrowed)?;
    sender.flush_video();
    let recovered = sender.reclaim_video().unwrap();
    assert_eq!(recovered.data().as_ptr(), pixels);
    assert_eq!(recovered.data()[0], 4);
    assert!(sender.reclaim_video().is_none());
    sender.submit_video(frame(5))?;
    drop(sender);
    Ok(())
}

#[test]
fn concurrent_sync_and_flush_preserve_owned_frame() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let options = SenderOptions::new(common::unique_name("sync and flush regression"))
        .clock_video(false)
        .clock_audio(false);
    let mut sender = Sender::new(&ndi, &options)?;
    let submitted = frame(6);
    let original_ptr = submitted.data().as_ptr();
    sender.submit_video(submitted)?;
    let shared = Arc::new(sender);
    std::thread::scope(|scope| {
        scope.spawn(|| shared.flush_video());
        scope.spawn(|| shared.send_video(&frame(7)).unwrap());
    });
    let mut sender = Arc::try_unwrap(shared).ok().unwrap();
    let returned = sender.reclaim_video().unwrap();
    assert_eq!(returned.data().as_ptr(), original_ptr);
    assert_eq!(returned.metadata(), Some("<frame value=\"6\"/>"));
    assert!(sender.reclaim_video().is_none());
    Ok(())
}

/// One sender's async sends, flushes, finish, and a drop with a frame still in
/// flight, run by `senders_on_separate_threads_flush_independently`. Returns
/// the first byte of the frame `reclaim_video` recovers.
fn flush_repeatedly(mut sender: Sender, start: &Barrier) -> grafton_ndi::Result<Option<u8>> {
    start.wait();
    for value in 1..=FLUSH_ROUNDS {
        sender.submit_video(frame(value))?;
        sender.flush_video();
    }
    let recovered = sender.reclaim_video().map(|frame| frame.data()[0]);
    sender.submit_video(frame(0))?;
    drop(sender);
    Ok(recovered)
}

const FLUSH_ROUNDS: u8 = 200;

#[test]
fn senders_on_separate_threads_flush_independently() -> Result<(), grafton_ndi::Error> {
    const SENDERS: usize = 4;
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let start = Arc::new(Barrier::new(SENDERS));
    let (done, finished) = mpsc::channel();
    for index in 0..SENDERS {
        let name = common::unique_name(&format!("concurrent flush {index}"));
        let options = SenderOptions::new(name)
            .clock_video(false)
            .clock_audio(false);
        let sender = Sender::new(&ndi, &options)?;
        let (start, done) = (start.clone(), done.clone());
        // Detached rather than scoped, so a flush that never returns fails the
        // watchdog below instead of hanging the join.
        thread::spawn(move || {
            let _ = done.send(flush_repeatedly(sender, &start));
        });
    }
    drop(done);
    for _ in 0..SENDERS {
        let recovered = finished
            .recv_timeout(Duration::from_secs(30))
            .expect("a sender's flush did not return")?;
        assert_eq!(recovered, Some(FLUSH_ROUNDS));
    }
    Ok(())
}

#[test]
fn copied_send_retains_metadata_after_source_drop() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let options = SenderOptions::new(common::unique_name("copied metadata lifetime regression"))
        .clock_video(false)
        .clock_audio(false);
    let mut sender = Sender::new(&ndi, &options)?;
    let source = frame(8);
    sender.submit_video_copy(&source)?;
    drop(source);
    // Both source pixels and metadata have been released before the SDK drain.
    let recovered = sender.reclaim_video().unwrap();
    assert_eq!(recovered.data()[0], 8);
    assert_eq!(recovered.metadata(), Some("<frame value=\"8\"/>"));
    Ok(())
}

/// Clocked video remains on its submitting thread while independently paced
/// audio reaches the same receiver through the shared half.
#[test]
fn split_clocked_video_keeps_audio_flowing_on_another_thread() -> grafton_ndi::Result<()> {
    use grafton_ndi::{AudioFrame, FrameRate, Receiver, ReceiverOptions};
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Instant,
    };
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let mut sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("raw split audio"))
            .clock_video(true)
            .clock_audio(false),
    )?;
    let source = common::loopback_source(&ndi, &sender)?;
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    let warm = AudioFrame::builder()
        .num_channels(1)
        .num_samples(480)
        .build()?;
    let ready = AtomicBool::new(false);
    struct Ready<'a>(&'a AtomicBool);
    impl Drop for Ready<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    thread::scope(|scope| {
        scope.spawn(|| {
            while !ready.load(Ordering::Acquire) {
                sender.send_audio(&warm);
                thread::sleep(Duration::from_millis(10));
            }
        });
        let _ready = Ready(&ready);
        let deadline = Instant::now() + Duration::from_secs(15);
        while receiver
            .capture_audio(Duration::from_millis(100))
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline, "audio receiver did not connect");
        }
    });
    let video_frame = VideoFrame::builder()
        .resolution(16, 16)
        .frame_rate(FrameRate::new(30, 1)?)
        .build()?;
    let audio: Vec<_> = (1..=32)
        .map(|index| {
            AudioFrame::builder()
                .num_channels(1)
                .num_samples(480)
                .data(vec![index as f32 / 100.0; 480])
                .build()
                .unwrap()
        })
        .collect();
    let (mut video, shared) = sender.split();
    thread::scope(|scope| {
        scope.spawn(move || {
            for _ in 0..12 {
                video.submit_video_copy(&video_frame).unwrap();
            }
            video.reclaim_video();
        });
        scope.spawn(move || {
            for frame in &audio {
                shared.send_audio(frame);
                thread::sleep(Duration::from_millis(10));
            }
        });
        let mut received = [false; 32];
        let deadline = Instant::now() + Duration::from_secs(15);
        while received.iter().any(|seen| !seen) {
            assert!(
                Instant::now() < deadline,
                "audio frames missing during clocked video: {received:?}"
            );
            if let Some(frame) = receiver.capture_audio(Duration::from_millis(100)).unwrap() {
                for &sample in frame.data() {
                    let index = (sample * 100.0).round() as usize;
                    if (1..=32).contains(&index) {
                        received[index - 1] = true;
                    }
                }
            }
        }
    });
    Ok(())
}
