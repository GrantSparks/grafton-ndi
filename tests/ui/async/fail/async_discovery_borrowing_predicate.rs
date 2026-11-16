#[cfg(any(feature = "tokio", feature = "smol"))]
async fn contract<R: grafton_ndi::asynchronous::AsyncRuntime>(
    finder: &grafton_ndi::asynchronous::AsyncFinder<R>,
) {
    let name = String::from("camera");
    finder
        .find_source(std::time::Duration::ZERO, |source| source.name() == name)
        .await
        .unwrap();
}
fn main() {}
