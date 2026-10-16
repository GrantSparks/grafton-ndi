use grafton_ndi::VideoFrameRef;

fn escape<'a>(frame: VideoFrameRef<'a>) -> impl Sized + 'a {
    frame.plane(0).unwrap().rows()
}

fn main() {}
