//! Discovery Server monitoring and explicitly enabled remote control.
//!
//! These APIs are separate from ordinary Finder discovery. Native owners are
//! thread-affine; copied snapshots and events can be sent to other threads.

mod advertiser;
mod listener;
pub use advertiser::{
    AdvertiserOptions, RecvAdvertisement, RecvAdvertiser, RecvRegistration, SendAdvertisement,
    SendAdvertiser, SendRegistration,
};
pub use listener::{
    AdvertisedId, AdvertisedReceiver, AdvertisedSender, ListenerEvent, ListenerOptions,
    ReceiverCommand, ReceiverStream, RecvListener, SendListener, Wait,
};
