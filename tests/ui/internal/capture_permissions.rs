//! Compiled inside receiver.rs by the internal_capture_permissions test. Privacy
//! is therefore not a reason for failure: each call must fail its mode bound.

use super::*;

fn attached_audio(receiver: &Receiver<FrameSyncAttached>, deadline: Deadline) {
    let _ = receiver.capture_ref_until::<AudioKind>(deadline, NeverStop);
}

fn attached_mixed(receiver: &Receiver<FrameSyncAttached>, deadline: Deadline) {
    let _ = receiver.capture_any_ref_until(deadline, NeverStop);
}

fn attached_events(receiver: &Receiver<FrameSyncAttached>, deadline: Deadline) {
    let _ = receiver.poll_event_until(deadline, NeverStop);
}

fn attached_audio_view(receiver: &Receiver<FrameSyncAttached>) {
    let _ = Capture::<AudioKind>::new(receiver);
}
