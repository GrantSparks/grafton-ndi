use grafton_ndi::advanced::VideoClock;

fn duplicate(clock: &VideoClock<'_>) {
    let _: VideoClock<'_> = clock.clone();
}
fn main() {}
