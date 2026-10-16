use grafton_ndi::CaptureKind;
fn raw<K: CaptureKind>() {
    let _ = K::FRAME_TYPE;
}
fn main() {}
