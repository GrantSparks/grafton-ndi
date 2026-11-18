//! Compiled inside packets, outside its payload module.
#![allow(dead_code)]
use super::*;

#[cfg(ndi_test_packet_payload)]
fn construct(bytes: super::super::aliasable::AliasableVec<u8>, len: PacketLen) -> PacketBytes {
    PacketBytes {
        bytes,
        len,
        extents: None,
    }
}

#[cfg(ndi_test_packet_payload_mutation)]
fn alter_extent(packet: &mut PacketBytes, len: PacketLen) {
    packet.len = len;
}

#[cfg(ndi_test_packet_payload)]
fn construct_slice(bytes: &[u8], len: PacketLen) -> PacketSlice<'_> {
    PacketSlice { bytes, len }
}

#[cfg(ndi_test_packet_payload)]
fn construct_frame(
    format: VideoPacketFormat,
    payload: PacketBytes,
    extras: FrameExtras,
) -> CompressedFrame<VideoPacketFormat> {
    CompressedFrame {
        format,
        payload,
        extras,
    }
}
