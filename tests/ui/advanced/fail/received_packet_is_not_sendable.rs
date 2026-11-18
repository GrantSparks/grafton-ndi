use grafton_ndi::advanced::{CompressedSender, ReceivedPacket, VideoPacketFormat};
fn send(sender: &mut CompressedSender, packet: &mut ReceivedPacket<VideoPacketFormat>) {
    sender.send_video(packet);
}
fn main() {}
