use grafton_ndi::advanced::{CompressedSender, ReceivedVideoPacket};
fn send(sender: &mut CompressedSender, packet: &mut ReceivedVideoPacket) {
    sender.send_video(packet);
}
fn main() {}
