use grafton_ndi::{
    advanced::{AllocatedPacketReceiver, AllocatedReceiver, AvSync, PacketReceiver},
    FrameSync, Source,
};

fn framesync(sync: &mut FrameSync, next: &Source) {
    let session = sync.controls().kvm().unwrap();
    session.receiver().connect_to(next).unwrap();
}

fn avsync(sync: &mut AvSync) {
    let session = sync.controls().kvm().unwrap();
    session.receiver().disconnect();
}

fn packets(packets: &mut PacketReceiver) {
    let session = packets.controls().kvm().unwrap();
    session.receiver().reconnect().unwrap();
}

fn allocated(receiver: &mut AllocatedReceiver, next: &Source) {
    let session = receiver.controls().kvm().unwrap();
    session.receiver().connect_to(next).unwrap();
}

fn allocated_packets(packets: &mut AllocatedPacketReceiver) {
    let session = packets.controls().kvm().unwrap();
    session.receiver().disconnect();
}

fn main() {}
