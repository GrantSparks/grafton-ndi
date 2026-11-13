use grafton_ndi::CaptureKind;
fn raw<K: CaptureKind>() {
    let _ = K::FRAME_TYPE;
}
fn copy<K: CaptureKind>(frame: &K::Ref<'_>) -> K::Owned {
    K::ref_to_owned(frame)
}
fn main() {}
