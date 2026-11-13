#![allow(unused)]
#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use grafton_ndi::{CaptureKind, ReceiverItemRef, VideoFrame};
    use std::time::Duration;
    let summary: Option<(u32, u32)> = receiver
        .video()
        .capture_with(Duration::ZERO, |frame| (frame.width(), frame.height()))
        .await
        .unwrap();
    let owned: Option<VideoFrame> = receiver
        .video()
        .capture_with(Duration::ZERO, |frame| frame.to_owned())
        .await
        .unwrap();
    let samples = receiver
        .audio()
        .capture_with(Duration::ZERO, |frame| {
            frame.channel_data(0).map(|c| c.iter().sum::<f32>())
        })
        .await
        .unwrap();
    let text: Option<String> = receiver
        .metadata()
        .capture_with(Duration::ZERO, |frame| frame.data().to_owned())
        .await
        .unwrap();
    let once = String::from("consumed by an FnOnce callback");
    receiver
        .video()
        .capture_with(Duration::ZERO, move |frame| {
            drop(once);
            frame.width()
        })
        .await
        .unwrap();
    let mixed = receiver
        .capture_any_with(Duration::ZERO, |item| item.to_owned())
        .await
        .unwrap();
    async fn generic<R: grafton_ndi::asynchronous::AsyncRuntime, K: CaptureKind>(
        capture: &grafton_ndi::asynchronous::AsyncCapture<'_, R, K>,
    ) where
        K::Owned: Send + 'static,
    {
        capture.capture_with(Duration::ZERO, |_| 42).await.unwrap();
    }
}
fn main() {}
