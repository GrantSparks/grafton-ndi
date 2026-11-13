use grafton_ndi::BorrowedAudioFrame;
fn main() {
    let samples = [0.0; 4];
    let metadata = std::ffi::CString::new("<audio/>").unwrap();
    let frame = BorrowedAudioFrame::builder(&samples, 48_000, 2, 2)
        .metadata(&metadata)
        .build()
        .unwrap();
    drop(metadata);
    let _ = frame.to_owned();
}
