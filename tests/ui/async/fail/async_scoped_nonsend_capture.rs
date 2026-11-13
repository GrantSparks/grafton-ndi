#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) {
    use std::time::Duration;
    let local = std::rc::Rc::new(42);
    receiver
        .video()
        .capture_with(Duration::ZERO, move |_| *local)
        .await
        .unwrap();
}
fn main() {}
