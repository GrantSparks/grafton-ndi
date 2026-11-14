//! Native loopback coverage for items arriving after an abandoned attempt ends.
#![cfg(any(feature = "tokio", feature = "smol"))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    Error, Ndi, PixelFormat, Receiver, ReceiverOptions, RuntimeFeature, Sender, SenderOptions,
    Source, SourceNameChange, VideoFrame,
};
use std::{
    future::Future,
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
    std::thread::sleep(Duration::from_millis(150));
}

async fn later_items<R: AsyncRuntime>(ndi: &Ndi, runtime: &str) -> Result<(), Error> {
    let sender = Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(&format!("cancel {runtime}")))
            .clock_video(false)
            .clock_audio(false),
    )?;
    let source = common::loopback_source(ndi, &sender)?;
    let receiver =
        AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(source.clone()))?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receiver.is_connected() {
        assert!(Instant::now() < deadline, "receiver did not connect");
        std::thread::sleep(Duration::from_millis(10));
    }
    let video = VideoFrame::builder()
        .resolution(16, 16)
        .pixel_format(PixelFormat::Bgra)
        .data(vec![37; 16 * 16 * 4])
        .build()?;
    for round in 0..10 {
        abandon(receiver.video().capture(Duration::from_secs(4)));
        sender.send_video(&video)?;
        let frame = receiver.video().capture(Duration::from_secs(2)).await?;
        assert!(
            frame.is_some(),
            "{runtime} lost round {round} after the abandoned attempt ended"
        );
    }
    if ndi
        .require_feature(RuntimeFeature::ConnectedSourceName)
        .is_ok()
    {
        let receiver =
            AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(Source::new("")))?);
        while receiver
            .poll_source_name_change(Duration::ZERO)
            .await?
            .is_some()
        {}
        abandon(receiver.poll_source_name_change(Duration::from_secs(4)));
        receiver.connect_to(&source).await?;
        let change = receiver
            .poll_source_name_change(Duration::from_secs(3))
            .await?;
        assert_eq!(
            change,
            Some(SourceNameChange::Connected(source.name().to_owned()))
        );
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
