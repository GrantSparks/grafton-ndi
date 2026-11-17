//! Compiled as a sibling of receiver, at its actual privacy boundary.

fn receiver_handle(core: &crate::receiver::ReceiverCore) {
    let _ = core.instance;
}

fn opaque_identity(instance: crate::receiver::RecvInstance) {
    let _ = instance.0;
}
