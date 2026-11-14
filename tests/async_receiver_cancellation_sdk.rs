//! Native loopback cancellation, worker completion and subsequent media capture.
#![cfg(any(feature = "tokio", feature = "smol"))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    Error, Ndi, PixelFormat, Receiver, ReceiverOptions, Sender, SenderOptions, Timecode,
    VideoFrame,
};
use std::{
    future::Future,
    sync::mpsc,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

fn abandon(future: impl Future) {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    // Give the blocking work time to enter its first attempt, then abandon it.
    std::thread::sleep(Duration::from_millis(30));
    drop(future);
}

/// Owned by the callback on the blocking worker, including when a cancelled
/// wait returns without an item and drops the unused callback there.
struct WorkerFinished(mpsc::Sender<()>);

impl Drop for WorkerFinished {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

async fn later_items<R: AsyncRuntime>(ndi: &Ndi, runtime: &str) -> Result<(), Error> {
    let sender = Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(&format!("cancel {runtime}")))
            .clock_video(false)
            .clock_audio(false),
    )?;
    let source = common::loopback_source(ndi, &sender)?;
    let receiver = AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(source))?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receiver.is_connected() {
        assert!(Instant::now() < deadline, "receiver did not connect");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut video = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::Bgra)
        .data(vec![37; 16 * 16 * 4])
        .build()?;
    for round in 0..10 {
        let (finished, done) = mpsc::channel();
        let finished = WorkerFinished(finished);
        abandon(
            receiver
                .video()
                .capture_with(Duration::from_secs(4), move |_| {
                    drop(finished);
                }),
        );
        // Native waits and worker scheduling can overrun a nominal 100 ms
        // attempt. A sleep alone cannot prove the abandoned attempt ended.
        // The original bug runs to its four-second deadline and fails here.
        done.recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("{runtime} retained its abandoned worker: {error}"));

        // Establish the worker is gone before sending. Retry live exchange:
        // one unclocked native frame can be lost independently of cancellation.
        let timecode = Timecode::from_ticks(round);
        video.set_timecode(timecode);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            sender.send_video(&video)?;
            let frame = receiver.video().capture(Duration::from_millis(100)).await?;
            if frame.is_some_and(|frame| frame.timecode() == timecode) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{runtime} did not receive live video for round {round} after its worker ended"
            );
        }
    }
    Ok(())
}

#[test]
fn abandoned_native_waits_leave_later_items_for_live_callers() -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    #[cfg(feature = "tokio")]
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(later_items::<grafton_ndi::asynchronous::Tokio>(
            &ndi, "tokio",
        ))?;
    #[cfg(feature = "smol")]
    smol::block_on(later_items::<grafton_ndi::asynchronous::Smol>(&ndi, "smol"))?;
    Ok(())
}
