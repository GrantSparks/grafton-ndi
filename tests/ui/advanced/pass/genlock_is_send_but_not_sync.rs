// A Genlock can move to another thread, and its clocks can each be waited on
// from a thread of their own, but the genlock itself is not Sync: the SDK
// documents only the two waits as safe on separate threads at once, not
// `is_active`. The check below compiles only while Sync is NOT implemented:
// were it implemented, both impls would apply and the call would be ambiguous
// (E0283). A failing fixture would print the native handle's type, which
// differs between builds with and without the Advanced SDK headers.
use grafton_ndi::advanced::{AudioClock, Genlock, VideoClock};

trait AmbiguousIfSync<A> {
    fn check() {}
}
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}

fn send<T: Send>() {}

fn main() {
    send::<Genlock>();
    <Genlock as AmbiguousIfSync<_>>::check();
    send::<VideoClock<'static>>();
    send::<AudioClock<'static>>();
}
