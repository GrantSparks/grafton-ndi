use grafton_ndi::advanced::QueuedSender;

fn require_send<T: Send>() {}
fn main() {
    require_send::<QueuedSender>();
}
