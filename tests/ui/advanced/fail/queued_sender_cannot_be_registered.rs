use grafton_ndi::{
    advanced::QueuedSender,
    discovery_server::{SendAdvertisement, SendAdvertiser},
};
fn register(advertiser: &mut SendAdvertiser, sender: &QueuedSender) {
    let _ = advertiser.register(sender.sender(), &SendAdvertisement::default());
}
fn main() {}
