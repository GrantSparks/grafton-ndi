use grafton_ndi::{
    advanced::{
        AllocatedReceiver, AllocationBuffer, PacketAttached, PacketReceiver, ReceiverControls,
    },
    Receiver, ReceiverMode,
};

fn shared<M: ReceiverMode>(receiver: &mut AllocatedReceiver<M>) {
    let _: &Receiver<M> = receiver.receiver();
    let _: ReceiverControls<'_, M> = receiver.controls();
    receiver
        .set_video_allocator(AllocationBuffer::zeroed)
        .unwrap();
    receiver
        .set_audio_allocator(AllocationBuffer::zeroed)
        .unwrap();
    receiver.reset_video_allocator();
    receiver.reset_audio_allocator();
}

fn raw(receiver: AllocatedReceiver) -> Receiver {
    receiver.into_receiver()
}

fn packets(receiver: AllocatedReceiver<PacketAttached>) -> PacketReceiver {
    receiver.into_packet_receiver()
}

fn main() {
    let _ = (
        shared::<grafton_ndi::RawCapture>,
        shared::<PacketAttached>,
        raw,
        packets,
    );
}
