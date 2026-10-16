use grafton_ndi::FrameSync;
fn check(owner: FrameSync) {
    let frame = owner
        .capture_video(grafton_ndi::ScanType::Progressive)
        .unwrap()
        .unwrap();
    let planes = frame.planes();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            planes
                .flat_map(|p| p.rows())
                .flatten()
                .map(|&v| u64::from(v))
                .sum::<u64>()
        });
        drop(owner);
        let _ = worker.join().unwrap();
    });
}
fn main() {}
