// The synchronizer reads its receiver's native instance, so an AvSync never
// lends its Receiver mutably, where it could be swapped for another AvSync's.
use grafton_ndi::advanced::AvSync;
fn swap(first: &mut AvSync, second: &mut AvSync) {
    std::mem::swap(first.receiver_mut(), second.receiver_mut());
}
fn main() {}
