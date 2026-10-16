//! Live padding regression. Run alone against the standard SDK, optionally under
//! Valgrind: `--tool=memcheck --track-origins=yes --error-exitcode=99`.
#![cfg(target_os = "linux")]

mod common;

use grafton_ndi::{
    BorrowedVideoFrame, FrameSync, LineStrideOrSize, PixelFormat, Receiver, ReceiverColorFormat,
    ReceiverOptions, ScanType, Sender, SenderOptions, Source, VideoFrame, NDI,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn receiver(ndi: &NDI, source: Source) -> Receiver {
    Receiver::new(
        ndi,
        &ReceiverOptions::builder(source)
            .color(ReceiverColorFormat::BGRX_BGRA)
            .allow_video_fields(false)
            .build(),
    )
    .unwrap()
}

fn verify_owned(frame: &VideoFrame) {
    assert_eq!((frame.width(), frame.height()), (642, 32));
    assert_eq!(
        frame.line_stride_or_size(),
        LineStrideOrSize::LineStrideBytes(2568)
    );
    assert_eq!(frame.data().len(), 82_176);
    assert_eq!(frame.metadata(), Some("<padding-regression/>"));
    // System calls make uninitialized values observable to Memcheck.
    std::fs::write("/dev/null", frame.data()).unwrap();
    #[cfg(feature = "image-encoding")]
    {
        std::fs::write("/dev/null", frame.encode_png().unwrap()).unwrap();
        std::fs::write("/dev/null", frame.encode_jpeg(85).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "Requires NDI runtime and loopback networking; run alone"]
fn live_receiver_framesync_and_owned_send_skip_padding() {
    let ndi = NDI::new().unwrap();
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder("video padding regression")
            .clock_video(true)
            .clock_audio(false)
            .build(),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let worker_stop = stop.clone();
        scope.spawn(move || {
            let mut frame = VideoFrame::builder()
                .resolution(642, 32)
                .pixel_format(PixelFormat::BGRA)
                .metadata("<padding-regression/>")
                .build()
                .unwrap();
            for pixel in frame.data_mut().chunks_exact_mut(4) {
                pixel.copy_from_slice(&[40, 80, 120, 255]);
            }
            while !worker_stop.load(Ordering::Relaxed) {
                sender.send_video(&frame);
            }
        });
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _stop = Stop(stop);
        let receiver = receiver(&ndi, source);
        let deadline = Instant::now() + Duration::from_secs(30);
        let borrowed = loop {
            assert!(
                Instant::now() < deadline,
                "receiver did not produce a frame"
            );
            if let Some(frame) = receiver
                .video()
                .try_capture_ref(Duration::from_millis(100))
                .unwrap()
            {
                break frame;
            }
        };
        eprintln!("receiver SDK stride: {:?}", borrowed.line_stride_or_size());
        assert!(borrowed.data().is_none(), "expected padded SDK output");
        let mut active = 0;
        for plane in borrowed.planes() {
            for row in plane.rows() {
                active += row.len();
                std::fs::write("/dev/null", row).unwrap();
            }
        }
        assert_eq!(active, 82_176);
        #[cfg(feature = "image-encoding")]
        {
            std::fs::write("/dev/null", borrowed.encode_png().unwrap()).unwrap();
            std::fs::write("/dev/null", borrowed.encode_jpeg(85).unwrap()).unwrap();
        }
        let owned = borrowed.to_owned().unwrap();
        verify_owned(&owned);
        drop(borrowed);
        let owned_capture = receiver.video().capture(Duration::from_secs(10)).unwrap();
        verify_owned(&owned_capture);
        #[cfg(feature = "tokio")]
        let receiver = {
            use grafton_ndi::tokio::AsyncReceiver;
            // Use a separate receiver because AsyncReceiver retains its owner.
            let async_receiver =
                AsyncReceiver::new(self::receiver(&ndi, receiver.source().clone()));
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let frame = runtime
                .block_on(async_receiver.video().capture(Duration::from_secs(10)))
                .unwrap();
            verify_owned(&frame);
            receiver
        };
        #[cfg(feature = "async-std")]
        {
            use grafton_ndi::async_std::AsyncReceiver;
            let async_receiver =
                AsyncReceiver::new(self::receiver(&ndi, receiver.source().clone()));
            let frame =
                async_std::task::block_on(async_receiver.video().capture(Duration::from_secs(10)))
                    .unwrap();
            verify_owned(&frame);
        }
        let framesync = FrameSync::new(receiver).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                Instant::now() < deadline,
                "FrameSync did not produce a frame"
            );
            if let Some(frame) = framesync.capture_video(ScanType::Progressive).unwrap() {
                eprintln!("FrameSync SDK stride: {:?}", frame.line_stride_or_size());
                for plane in frame.planes() {
                    for row in plane.rows() {
                        std::fs::write("/dev/null", row).unwrap();
                    }
                }
                #[cfg(feature = "image-encoding")]
                {
                    std::fs::write("/dev/null", frame.encode_png().unwrap()).unwrap();
                    std::fs::write("/dev/null", frame.encode_jpeg(85).unwrap()).unwrap();
                }
                verify_owned(&frame.to_owned().unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        verify_owned(
            &framesync
                .capture_video_owned(ScanType::Progressive)
                .unwrap()
                .unwrap(),
        );
        let _ = framesync
            .receiver()
            .metadata()
            .try_capture(Duration::ZERO)
            .unwrap();
        let _ = framesync
            .receiver()
            .poll_status_change(Duration::ZERO)
            .unwrap();
        let receiver = framesync.into_receiver();
        verify_owned(&receiver.video().capture(Duration::from_secs(10)).unwrap());
        let mut resend = Sender::new(
            &ndi,
            &SenderOptions::builder("normalized padding resend")
                .clock_video(false)
                .clock_audio(false)
                .build(),
        )
        .unwrap();
        let original = owned.data().as_ptr();
        assert!(resend.send_video_async(owned).is_none());
        let borrowed_send = BorrowedVideoFrame::from(&owned_capture);
        let recovered = resend
            .send_video_async_copy(&borrowed_send)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.data().as_ptr(), original);
        verify_owned(&recovered);
        verify_owned(&resend.finish_video_async().unwrap());
    });
}
