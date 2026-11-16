#[cfg(feature = "tokio")]
fn contract(watch: grafton_ndi::asynchronous::tokio::AsyncSourceWatch) {
    ::tokio::spawn(async move {
        let mut watch = watch;
        watch.current_sources().await.unwrap();
        watch
            .wait_for_change(std::time::Duration::ZERO)
            .await
            .unwrap();
    });
}
fn main() {}
