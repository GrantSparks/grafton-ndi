#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use std::time::Duration;
    receiver
        .video()
        .capture_with(Duration::ZERO, |_| std::rc::Rc::new(42))
        .await
        .unwrap();
}
fn main() {}
