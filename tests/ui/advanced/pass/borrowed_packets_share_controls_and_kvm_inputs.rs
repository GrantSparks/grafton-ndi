use grafton_ndi::{advanced::*, Tally};
use std::{
    panic::{RefUnwindSafe, UnwindSafe},
    time::Duration,
};
fn sharing(receiver: &PacketReceiver) {
    let packet = receiver.capture_any_ref(Duration::ZERO).unwrap();
    receiver.receiver().set_tally(Tally::new(true, false));
    let _ = receiver.receiver().request_keyframe();
    drop(packet);
}
fn kvm(receiver: &mut PacketReceiver) {
    let mut session = receiver.controls().kvm().unwrap();
    let packet = session.receiver().capture_any_ref(Duration::ZERO).unwrap();
    session.key_down(0xffe1).unwrap();
    session.release_all().unwrap();
    drop(session);
    drop(packet);
}
// A packet view can always be shortened to the borrow that consumes it.
fn shorter_video<'long: 'short, 'short>(frame: CompressedRef<'long, VideoPacketFormat>) -> CompressedRef<'short, VideoPacketFormat> {
    frame
}
fn shorter_audio<'long: 'short, 'short>(frame: CompressedRef<'long, AudioPacketFormat>) -> CompressedRef<'short, AudioPacketFormat> {
    frame
}
fn main() {
    fn traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}
    traits::<PacketItemRef<'static>>();
    traits::<CompressedRef<'static, VideoPacketFormat>>();
    traits::<CompressedRef<'static, AudioPacketFormat>>();
    traits::<ReceivedVideoRef<'static>>();
    traits::<ReceivedAudioRef<'static>>();
}
