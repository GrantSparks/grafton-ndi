use grafton_ndi::Sender;
fn overlap(sender: &mut Sender) {
    let (mut video, _) = sender.split();
    sender.flush_video();
    video.reclaim_video();
}
fn main() {}
