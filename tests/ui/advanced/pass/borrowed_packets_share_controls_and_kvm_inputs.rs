use grafton_ndi::{advanced::*, Tally};
use std::{
    panic::{RefUnwindSafe, UnwindSafe},
    time::Duration,
};
fn sharing(receiver: &PacketReceiver) {
    let packet = receiver.capture_ref(Duration::ZERO).unwrap();
    receiver.receiver().set_tally(Tally::new(true, false));
    let _ = receiver.receiver().request_keyframe();
    drop(packet);
}
fn kvm(receiver: &mut PacketReceiver) {
    let mut session = receiver.controls().kvm().unwrap();
    let packet = session.capture_ref(Duration::ZERO).unwrap();
    session.key_down(0xffe1).unwrap();
    session.release_all().unwrap();
    drop(session);
    drop(packet);
}
fn main() {
    fn traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}
    traits::<PacketItemRef<'static>>();
    traits::<CompressedVideoRef<'static>>();
    traits::<CompressedAudioRef<'static>>();
    traits::<ReceivedVideoRef<'static>>();
    traits::<ReceivedAudioRef<'static>>();
}
