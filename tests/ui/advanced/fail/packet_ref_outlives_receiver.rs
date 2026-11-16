use grafton_ndi::{
    advanced::{PacketItemRef, PacketReceiver, PassthroughPreset},
    Ndi, ReceiverOptions, Source,
};
use std::time::Duration;
fn escape() -> PacketItemRef<'static> {
    let ndi = Ndi::new().unwrap();
    let receiver = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(Source::new("").unwrap()),
        PassthroughPreset::Hevc,
    )
    .unwrap();
    receiver.capture_any_ref(Duration::ZERO).unwrap().unwrap()
}
fn main() {}
