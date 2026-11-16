use grafton_ndi::{RawVideo, Sender};
fn escape(mut sender: Sender) -> RawVideo<'static> {
    sender.split().0
}
fn main() {}
