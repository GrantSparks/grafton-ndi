// An AvSync can move to another thread, but `AvSync::synchronize` takes
// `&self`, so it is not Sync: one call synchronizes at a time. The Receiver it
// lends is both. The check below compiles only while Sync is NOT implemented:
// were it implemented, both impls would apply and the call would be ambiguous
// (E0283). A failing fixture would print the native handle's type, which
// differs between builds with and without the Advanced SDK headers.
use grafton_ndi::{
    advanced::{AvSync, AvSyncAttached},
    Receiver,
};

trait AmbiguousIfSync<A> {
    fn check() {}
}
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}

fn send<T: Send>() {}
fn send_sync<T: Send + Sync>() {}

fn main() {
    send::<AvSync>();
    <AvSync as AmbiguousIfSync<_>>::check();
    send_sync::<Receiver<AvSyncAttached>>();
}
