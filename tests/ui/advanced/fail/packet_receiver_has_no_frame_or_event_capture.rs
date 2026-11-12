// Packet capture takes every frame and event, so the packet receiver's own
// Receiver has no capture of its own.
use grafton_ndi::advanced::PacketReceiver;
use std::time::Duration;
fn own_capture(packets: &PacketReceiver) {
    let _ = packets.receiver().video();
    let _ = packets.receiver().audio();
    let _ = packets.receiver().metadata();
    let _ = packets.receiver().poll_event(Duration::ZERO);
}
fn main() {}
