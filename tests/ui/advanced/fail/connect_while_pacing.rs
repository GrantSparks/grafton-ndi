use grafton_ndi::{advanced::Genlock, FrameRate, ScanType};

fn reconnect(genlock: &mut Genlock, frame_rate: FrameRate) {
    let (mut video, _audio) = genlock.clocks();
    genlock.connect(None).unwrap();
    let _ = video.wait(frame_rate, ScanType::Progressive);
}
fn main() {}
