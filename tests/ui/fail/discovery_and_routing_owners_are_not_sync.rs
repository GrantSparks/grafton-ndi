use grafton_ndi::{
    discovery::{RecvAdvertiser, RecvListener, SendAdvertiser, SendListener},
    routing::Router,
};
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn main() {
    assert_send::<(
        Router,
        SendAdvertiser,
        RecvAdvertiser,
        SendListener,
        RecvListener,
    )>();
    assert_sync::<Router>();
    assert_sync::<SendAdvertiser>();
    assert_sync::<RecvAdvertiser>();
    assert_sync::<SendListener>();
    assert_sync::<RecvListener>();
}
