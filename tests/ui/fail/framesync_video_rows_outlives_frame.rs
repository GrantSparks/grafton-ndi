use grafton_ndi::FrameSyncVideoRef;

fn escape<'a>(frame: FrameSyncVideoRef<'a>) -> impl Sized + 'a {
    frame.plane(0).unwrap().rows()
}

fn main() {}
