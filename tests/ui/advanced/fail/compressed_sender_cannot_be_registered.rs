use grafton_ndi::{
    advanced::CompressedSender,
    discovery::{SendAdvertisement, SendAdvertiser},
};
fn register(advertiser: &mut SendAdvertiser, sender: &CompressedSender) {
    let _ = advertiser.register(sender.sender(), SendAdvertisement::default());
}
fn main() {}
