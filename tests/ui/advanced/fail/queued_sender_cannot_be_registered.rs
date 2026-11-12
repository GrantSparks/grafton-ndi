use grafton_ndi::{
    advanced::QueuedSender,
    discovery::{SendAdvertisement, SendAdvertiser},
};
fn register(advertiser: &mut SendAdvertiser, sender: &QueuedSender) {
    let _ = advertiser.register(sender.sender(), SendAdvertisement::default());
}
fn main() {}
