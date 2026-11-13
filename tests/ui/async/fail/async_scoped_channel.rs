#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use std::time::Duration;
    receiver
        .audio()
        .capture_with(Duration::ZERO, |frame| frame.channel_data(0).unwrap())
        .await
        .unwrap();
}
fn main() {}
