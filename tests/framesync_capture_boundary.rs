use grafton_ndi::{Error, FrameSync, Receiver, ReceiverOptions, Source, SourceAddress, NDI};
use std::time::Duration;

#[test]
fn attached_receiver_rejects_raw_video_and_recovery_restores_capture() -> Result<(), Error> {
    let ndi = NDI::new()?;
    let source = Source {
        name: "FrameSync capture boundary".into(),
        address: SourceAddress::None,
    };
    let receiver = Receiver::new(&ndi, &ReceiverOptions::builder(source).build())?;
    let framesync = FrameSync::new(receiver)?;
    assert!(matches!(
        framesync.receiver().video().try_capture(Duration::ZERO),
        Err(Error::InvalidConfiguration(_))
    ));
    assert!(matches!(
        framesync.receiver().video().try_capture_ref(Duration::ZERO),
        Err(Error::InvalidConfiguration(_))
    ));
    // Controls and metadata remain accessible while FrameSync owns video capture.
    let _ = framesync
        .receiver()
        .metadata()
        .try_capture(Duration::ZERO)?;
    let receiver = framesync.into_receiver();
    assert!(!matches!(
        receiver.video().try_capture(Duration::ZERO),
        Err(Error::InvalidConfiguration(_))
    ));
    Ok(())
}
