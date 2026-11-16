// Packet capture takes every frame and event, so the packet receiver's own
// Receiver has no capture of its own.
use grafton_ndi::advanced::PacketReceiver;
use std::time::Duration;
fn own_capture(packets: &PacketReceiver) {
    let _ = packets.receiver().capture_video(Duration::ZERO);
    let _ = packets.receiver().capture_audio(Duration::ZERO);
    let _ = packets.receiver().capture_metadata(Duration::ZERO);
    let _ = packets.receiver().next_event(Duration::ZERO);
}
fn main() {}
