use grafton_ndi::FrameSync;

fn escape<'a>(owner: FrameSync) -> impl Sized + 'a {
    let frame = owner
        .capture_video(grafton_ndi::ScanType::Progressive)
        .unwrap()
        .unwrap();
    frame.plane(0).unwrap().rows()
}

fn main() {}
