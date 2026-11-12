// `AvSync::synchronize` takes `&self`, so one thread synchronizes, one call at
// a time: an AvSync is neither Send nor Sync, while the Receiver it lends is
// both. Each check below compiles only while the trait is NOT implemented:
// were it implemented, both impls would apply and the call would be
// ambiguous (E0283). A failing fixture would print the native handle's type,
// which differs between builds with and without the Advanced SDK headers.
use grafton_ndi::{
    advanced::{AvSync, AvSyncAttached},
    Receiver,
};

trait AmbiguousIfSend<A> {
    fn check() {}
}
impl<T: ?Sized> AmbiguousIfSend<()> for T {}
impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}

trait AmbiguousIfSync<A> {
    fn check() {}
}
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}

fn send_sync<T: Send + Sync>() {}

fn main() {
    <AvSync as AmbiguousIfSend<_>>::check();
    <AvSync as AmbiguousIfSync<_>>::check();
    send_sync::<Receiver<AvSyncAttached>>();
}
