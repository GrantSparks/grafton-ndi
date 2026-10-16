use grafton_ndi::Receiver;

fn escape<'a>(owner: Receiver) -> impl Sized + 'a {
    let frame = owner
        .video()
        .try_capture_ref(std::time::Duration::ZERO)
        .unwrap()
        .unwrap();
    frame.plane(0).unwrap().rows()
}

fn main() {}
