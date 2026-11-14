// A KVM session lends its receiver for the session's whole lifetime, so a
// captured frame can be held while keys, buttons and touches stay pressed, and
// a packet session captures through itself.
use std::time::Duration;

use grafton_ndi::{
    advanced::{AllocatedPacketReceiver, KvmReceiver, MouseButton, PacketAttached, PacketReceiver},
    CaptureMode, MetadataFrame, MetadataMode, Receiver, ReceiverMode, Source, Tally, VideoMode,
};

fn drag_across_frames(receiver: &mut Receiver) {
    let mut session = receiver.kvm().unwrap();
    let lent = session.receiver();
    let frame = lent.video().capture_ref(Duration::ZERO).unwrap();
    let audio = lent.audio().capture_ref(Duration::ZERO).unwrap();
    let metadata = lent.metadata().capture_ref(Duration::ZERO).unwrap();
    let mixed = session.receiver().capture_any_ref(Duration::ZERO).unwrap();
    session.key_down(0xffe1).unwrap();
    session.move_pointer(0.25, 0.25).unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    let next = session
        .receiver()
        .video()
        .capture_ref(Duration::ZERO)
        .unwrap();
    session.move_pointer(0.75, 0.75).unwrap();
    session.mouse_up(MouseButton::Left).unwrap();
    session.release_all().unwrap();
    let _ = (frame, next, audio, metadata, mixed);
}

fn packet_session(packets: &mut PacketReceiver) {
    let mut session = packets.controls().kvm().unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    let _event = session.capture(Duration::ZERO).unwrap();
    session.mouse_up(MouseButton::Left).unwrap();
    let _: KvmReceiver<'_, PacketAttached> = session.receiver();
}

fn allocated_packet_session(packets: &mut AllocatedPacketReceiver) {
    let mut session = packets.controls().kvm().unwrap();
    session.key_down(0xffe1).unwrap();
    let _ = session.capture(Duration::ZERO).unwrap();
    session.release_all().unwrap();
}

fn controls<M: ReceiverMode>(receiver: KvmReceiver<'_, M>) {
    receiver.set_tally(Tally::new(true, false));
    let metadata = MetadataFrame::new("<kvm/>").unwrap();
    let _ = receiver.send_metadata(&metadata);
    let _ = receiver.add_connection_metadata(&metadata);
    let _ = receiver.clear_connection_metadata();
    let _ = receiver.web_control_url();
    let _ = receiver.poll_source_name_change(Duration::ZERO);
    let _ = receiver.ptz().is_supported();
    let _ = receiver.is_connected();
    let _ = receiver.connection_target();
    let _ = receiver.connection_stats();
    let _ = receiver.request_keyframe();
    let _ = receiver.set_video_delay(Duration::ZERO);
    let _ = receiver.video_delay();
    let _ = receiver.set_audio_delay(Duration::ZERO);
    let _ = receiver.audio_delay();
}

fn video<M: VideoMode>(receiver: KvmReceiver<'_, M>) {
    let _ = receiver.video().capture_ref(Duration::ZERO);
}

fn audio<M: CaptureMode>(receiver: KvmReceiver<'_, M>) {
    let _ = receiver.audio().capture_ref(Duration::ZERO);
}

fn metadata<M: MetadataMode>(receiver: KvmReceiver<'_, M>) {
    let _ = receiver.metadata().capture_ref(Duration::ZERO);
    let _ = receiver.poll_event(Duration::ZERO);
}

fn scoped_controls_and_input(receiver: &mut Receiver) {
    let mut session = receiver.kvm().unwrap();
    let view = session.receiver();
    let ptz = view.ptz();
    std::thread::scope(|scope| {
        scope.spawn(move || controls(view));
        session.key_down(0xffe1).unwrap();
        let _ = ptz.is_supported();
        session.release_all().unwrap();
    });
}

fn switch(receiver: &mut Receiver, next: &Source) {
    let mut session = receiver.kvm().unwrap();
    session.key_down(0xffe1).unwrap();
    session.release_all().unwrap();
    drop(session);
    receiver.connect_to(next).unwrap();
    receiver.disconnect();
    receiver.reconnect().unwrap();
    let mut session = receiver.kvm().unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    session.release_all().unwrap();
}

fn main() {
    let _ = (
        drag_across_frames,
        packet_session,
        allocated_packet_session,
        scoped_controls_and_input,
        switch,
    );
    let _ = (
        controls::<grafton_ndi::RawCapture>,
        video::<grafton_ndi::RawCapture>,
        audio::<grafton_ndi::RawCapture>,
        metadata::<grafton_ndi::RawCapture>,
    );
}
