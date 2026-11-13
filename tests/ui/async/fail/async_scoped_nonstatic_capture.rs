#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use std::time::Duration;
    let local = 42;
    receiver
        .video()
        .capture_with(Duration::ZERO, |_| &local)
        .await
        .unwrap();
}
fn main() {}
