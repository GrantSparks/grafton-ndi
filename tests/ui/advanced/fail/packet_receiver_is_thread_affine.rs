use grafton_ndi::advanced::PacketReceiver;

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn main() {
    assert_send::<PacketReceiver>();
    assert_sync::<PacketReceiver>();
}
