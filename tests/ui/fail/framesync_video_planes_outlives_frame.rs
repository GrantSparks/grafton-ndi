use grafton_ndi::FrameSyncVideoRef;

fn escape<'a>(frame: FrameSyncVideoRef<'a>) -> impl Sized + 'a {
    frame.planes()
}

fn main() {}
