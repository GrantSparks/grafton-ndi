#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime);

use grafton_ndi::{Sender, SenderOptions};

#[test]
fn empty_name_uses_sdk_generated_sender_name() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let sender = Sender::new(&ndi, &SenderOptions::new(""))?;
    assert!(!sender.source()?.name().is_empty());
    Ok(())
}
