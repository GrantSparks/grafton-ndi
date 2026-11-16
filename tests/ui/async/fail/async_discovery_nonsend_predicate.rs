#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    finder: &grafton_ndi::asynchronous::AsyncFinder<R>,
) {
    let local = std::rc::Rc::new(42);
    finder
        .find_source(std::time::Duration::ZERO, move |_| *local == 42)
        .await
        .unwrap();
}
fn main() {}
