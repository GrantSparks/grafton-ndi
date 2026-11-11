use grafton_ndi::advanced::VideoClock;

fn require_sync<T: Sync>() {}
fn main() {
    require_sync::<VideoClock<'static>>();
}
