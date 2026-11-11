use grafton_ndi::{advanced::Genlock, ScanType};

fn reconnect(genlock: &mut Genlock) {
    let (mut video, _audio) = genlock.clocks();
    genlock.connect(None).unwrap();
    let _ = video.wait(30, 1, ScanType::Progressive);
}
fn main() {}
