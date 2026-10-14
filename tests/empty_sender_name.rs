use grafton_ndi::{Sender, SenderOptions, NDI};

#[test]
fn empty_name_uses_sdk_generated_sender_name() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let sender = Sender::new(&ndi, &SenderOptions::builder("").build())?;
    assert!(!sender.source()?.name.is_empty());
    Ok(())
}
