use grafton_ndi::Receiver;
fn check(owner: Receiver) {
    let frame = owner
        .video()
        .try_capture_ref(std::time::Duration::ZERO)
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
