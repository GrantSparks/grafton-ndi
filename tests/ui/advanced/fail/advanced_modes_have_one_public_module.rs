// Advanced modes have exactly one public module path.
use grafton_ndi::{
    AvSyncAttached, CompressedAttached, CompressedAudioSendMode, PacketAttached, QueueAttached,
};
use grafton_ndi::receiver::{AvSyncAttached as ReceiverAvSync, PacketAttached as ReceiverPackets};
use grafton_ndi::sender::{
    CompressedAttached as SenderCompressed, CompressedAudioSendMode as SenderCompressedAudio,
    QueueAttached as SenderQueued,
};

fn main() {}
