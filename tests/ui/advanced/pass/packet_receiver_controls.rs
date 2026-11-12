// A packet receiver's own Receiver, in the PacketAttached mode, keeps every
// common control a passthrough relay needs, with or without custom allocators,
// and can be advertised like any other receiver.
use grafton_ndi::{
    advanced::{AllocatedPacketReceiver, PacketAttached, PacketReceiver},
    discovery::{RecvAdvertisement, RecvAdvertiser},
    MediaTypes, MetadataFrame, Receiver, ReceiverMode, StreamTier, Tally,
};
fn common<M: ReceiverMode>(_: &Receiver<M>) {}
fn controls(receiver: &Receiver<PacketAttached>) {
    common(receiver);
    receiver.set_tally(Tally::new(true, false));
    let _ = receiver.connection_stats();
    let _ = receiver.ptz().is_supported();
    let _ = receiver.request_keyframe();
    let _ = receiver.send_metadata(&MetadataFrame::new("<relay/>").unwrap());
    let _ = receiver.reconnect();
}
fn packet_controls(packets: &mut PacketReceiver) {
    controls(packets.receiver());
    let _ = packets.receiver_mut().set_stream_tier(StreamTier::Preview);
    let _ = packets.receiver_mut().set_streams(MediaTypes::VIDEO);
}
fn allocated_controls(packets: &mut AllocatedPacketReceiver, advertiser: &mut RecvAdvertiser) {
    controls(packets.receiver());
    let _ = packets.set_stream_tier(StreamTier::Program);
    let _ = packets.set_streams(MediaTypes::VIDEO);
    let _ = advertiser.register(packets.receiver(), RecvAdvertisement::new());
}
fn main() {
    let _ = packet_controls;
    let _ = allocated_controls;
}
