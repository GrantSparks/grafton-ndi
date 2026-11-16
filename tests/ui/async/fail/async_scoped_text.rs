#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use std::time::Duration;
    receiver
        .capture_metadata_with(Duration::ZERO, |frame| frame.data())
        .await
        .unwrap();
}
fn main() {}
