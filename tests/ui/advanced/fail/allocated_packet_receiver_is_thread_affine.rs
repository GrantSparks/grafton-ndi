use grafton_ndi::advanced::AllocatedPacketReceiver;

// Attaching allocators to a PacketReceiver must not lift its thread
// restriction: the allocated receiver converts back into a PacketReceiver.
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn main() {
    assert_send::<AllocatedPacketReceiver>();
    assert_sync::<AllocatedPacketReceiver>();
}
