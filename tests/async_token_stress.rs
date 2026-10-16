use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, SenderOptions, VideoFrame, NDI};

fn frame(value: u8) -> VideoFrame {
    let mut frame = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::BGRA)
        .metadata(format!("<frame value=\"{value}\"/>"))
        .build()
        .unwrap();
    frame.data_mut().fill(value);
    frame
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn owned_pipeline_returns_original_buffers_and_metadata() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("owned pipeline regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let first = frame(1);
    let first_ptr = first.data().as_ptr();
    assert!(sender.send_video_async(first).is_none());
    let second = frame(2);
    let second_ptr = second.data().as_ptr();
    let recovered = sender.send_video_async(second).unwrap();
    assert_eq!(recovered.data().as_ptr(), first_ptr);
    assert!(recovered.data().iter().all(|byte| *byte == 1));
    assert_eq!(recovered.metadata(), Some("<frame value=\"1\"/>"));
    let recovered = sender.finish_video_async().unwrap();
    assert_eq!(recovered.data().as_ptr(), second_ptr);
    assert!(recovered.data().iter().all(|byte| *byte == 2));
    assert_eq!(recovered.metadata(), Some("<frame value=\"2\"/>"));
    assert!(sender.finish_video_async().is_none());
    Ok(())
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn copied_send_survives_released_source() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("copied source release regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let mut buffer = vec![7; 16 * 16 * 4];
    let borrowed =
        BorrowedVideoFrame::try_from_uncompressed(&buffer, 16, 16, PixelFormat::BGRA, 30, 1)?;
    let _ = sender.send_video_async_copy(&borrowed)?;
    buffer.fill(9);
    drop(buffer);
    let _ = sender.finish_video_async();
    Ok(())
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn mixed_owned_and_copy_send_completes_each_frame_once() -> Result<(), grafton_ndi::Error> {
    use std::sync::{Arc, Mutex};
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("mixed async regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let completed = Arc::new(Mutex::new(Vec::new()));
    let recorded = completed.clone();
    sender.on_async_video_done(move |len| recorded.lock().unwrap().push(len));
    let _ = sender.send_video_async(frame(3));
    let source = vec![4; 16 * 16 * 4];
    let borrowed =
        BorrowedVideoFrame::try_from_uncompressed(&source, 16, 16, PixelFormat::BGRA, 30, 1)?;
    let recovered = sender.send_video_async_copy(&borrowed)?.unwrap();
    assert_eq!(recovered.data()[0], 3);
    sender.flush_async_blocking();
    assert_eq!(completed.lock().unwrap().as_slice(), &[1024, 1024]);
    assert_eq!(sender.finish_video_async().unwrap().data()[0], 4);
    let _ = sender.send_video_async(frame(5));
    drop(sender);
    assert_eq!(completed.lock().unwrap().as_slice(), &[1024, 1024, 1024]);
    Ok(())
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn concurrent_sync_and_reentrant_flush_preserve_owned_frame() -> Result<(), grafton_ndi::Error> {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("sync and reentrant flush regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let submitted = frame(6);
    let original_ptr = submitted.data().as_ptr();
    let _ = sender.send_video_async(submitted);
    let reference = Arc::new(std::sync::Mutex::new(std::sync::Weak::<Sender>::new()));
    let weak_reference = reference.clone();
    let completions = Arc::new(AtomicUsize::new(0));
    let counted = completions.clone();
    sender.on_async_video_done(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        weak_reference
            .lock()
            .unwrap()
            .upgrade()
            .unwrap()
            .flush_async_blocking();
    });
    let shared = Arc::new(sender);
    *reference.lock().unwrap() = Arc::downgrade(&shared);
    std::thread::scope(|scope| {
        scope.spawn(|| shared.flush_async_blocking());
        scope.spawn(|| shared.send_video(&frame(7)));
    });
    assert_eq!(completions.load(Ordering::SeqCst), 1);
    let mut sender = Arc::try_unwrap(shared).ok().unwrap();
    let returned = sender.finish_video_async().unwrap();
    assert_eq!(returned.data().as_ptr(), original_ptr);
    assert_eq!(returned.metadata(), Some("<frame value=\"6\"/>"));
    assert_eq!(completions.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn panicking_completion_keeps_new_owned_frame_pending() -> Result<(), grafton_ndi::Error> {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("panicking completion regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let callbacks = Arc::new(AtomicUsize::new(0));
    let recorded = callbacks.clone();
    sender.on_async_video_done(move |_| {
        if recorded.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("intentional completion panic");
        }
    });
    let _ = sender.send_video_async(frame(1));
    let next = frame(2);
    let next_ptr = next.data().as_ptr();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = sender.send_video_async(next);
    }));
    assert!(panic.is_err());
    let recovered = sender.finish_video_async().unwrap();
    assert_eq!(recovered.data().as_ptr(), next_ptr);
    assert_eq!(recovered.metadata(), Some("<frame value=\"2\"/>"));
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
#[ignore = "Requires an installed NDI runtime"]
fn copied_send_retains_metadata_after_source_drop() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("copied metadata lifetime regression")
        .clock_video(false)
        .clock_audio(false)
        .build();
    let mut sender = Sender::new(&ndi, &options)?;
    let source = frame(8);
    let borrowed = BorrowedVideoFrame::from(&source);
    let _ = sender.send_video_async_copy(&borrowed)?;
    drop(source);
    // Both source pixels and metadata have been released before the SDK drain.
    let recovered = sender.finish_video_async().unwrap();
    assert_eq!(recovered.data()[0], 8);
    assert_eq!(recovered.metadata(), Some("<frame value=\"8\"/>"));
    Ok(())
}
