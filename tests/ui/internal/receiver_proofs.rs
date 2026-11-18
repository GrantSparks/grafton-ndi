//! Compiled as a sibling of receiver, at its actual privacy boundary.

fn receiver_handle(core: &crate::receiver::ReceiverCore) {
    let _ = core.instance;
}

fn opaque_identity(instance: crate::receiver::RecvInstance) {
    let _ = instance.0;
}

fn receiver_owned_controls_have_no_connection_access(
    control: &mut crate::receiver::control::Control<'_>,
    sdk: &crate::sys::Core,
    metadata: &crate::MetadataFrame,
    registration: &crate::sys::ReceiverConnectionMetadata,
    kvm: &crate::sys::advanced::Kvm,
    keyframe: &crate::sys::advanced::KeyframeRequest,
) {
    control.set_tally(sdk, crate::Tally::new(true, false));
    let _ = control.send_metadata(sdk, metadata);
    let _ = control.ptz_zoom(sdk, 0.5);
    control.add_metadata(registration, metadata);
    let _ = control.kvm_key_press(kvm, 1);
    control.request_keyframe(keyframe);
    let _ = control.instance;
}
