use grafton_ndi::{
    advanced::CompressedSender,
    discovery_server::{SendAdvertisement, SendAdvertiser},
};
fn register(advertiser: &mut SendAdvertiser, sender: &CompressedSender) {
    let _ = advertiser.register(sender.sender(), &SendAdvertisement::default());
}
fn main() {}
