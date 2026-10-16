use grafton_ndi::VideoFrameRef;

fn escape<'a>(frame: VideoFrameRef<'a>) -> impl Sized + 'a {
    frame.planes()
}

fn main() {}
