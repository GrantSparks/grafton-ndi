use grafton_ndi::RawVideo;
fn share(video: &RawVideo<'_>) {
    std::thread::scope(|scope| {
        scope.spawn(move || { let _ = format!("{video:?}"); });
    });
}
fn main() {}
