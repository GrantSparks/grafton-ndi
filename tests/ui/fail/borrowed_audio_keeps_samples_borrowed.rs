use grafton_ndi::BorrowedAudioFrame;
fn main() {
    let mut samples = [0.0; 4];
    let frame = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .build()
        .unwrap();
    samples.fill(1.0);
    let _ = frame.to_owned();
}
