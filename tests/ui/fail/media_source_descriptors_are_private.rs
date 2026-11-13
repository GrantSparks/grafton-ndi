use grafton_ndi::{AudioSource, VideoSource};

fn video<T: VideoSource>(frame: &T) {
    let _ = frame.pixels();
    let _ = frame.video_descriptor();
}

fn audio<T: AudioSource>(frame: &T) {
    let _ = frame.samples();
    let _ = frame.audio_descriptor();
}

fn main() {}
