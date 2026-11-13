use grafton_ndi::{
    advanced::{AvSyncAttached, PacketReceiver},
    Receiver,
};
use std::time::Duration;

fn packets(packets: &PacketReceiver) {
    let _ = packets.receiver().capture_any_ref(Duration::ZERO);
    let _ = packets.receiver().capture_any(Duration::ZERO);
}

fn synchronized(receiver: &Receiver<AvSyncAttached>) {
    let _ = receiver.capture_any_ref(Duration::ZERO);
    let _ = receiver.capture_any(Duration::ZERO);
}

fn main() {}
