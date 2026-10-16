use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, ScanType, Source,
    SourceAddress, Tally, NDI,
};
use std::time::Duration;

#[test]
fn attached_receiver_permits_controls_audio_metadata_and_raw_capture_after_recovery(
) -> Result<(), Error> {
    let ndi = NDI::new()?;
    let source = Source {
        name: "FrameSync capture boundary".into(),
        address: SourceAddress::None,
    };
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::builder(source).build())?;
    for _ in 0..3 {
        let framesync = FrameSync::new(receiver)?;
        let _video = framesync.capture_video(ScanType::Progressive)?;
        drop(_video);
        let _audio =
            framesync.capture_audio(FrameSyncAudioRequest::capture_with(Some(48_000), 2, 32)?)?;
        drop(_audio);
        let _ = framesync.receiver().set_tally(&Tally::new(false, true));
        assert!(!framesync.receiver().ptz_is_supported());
        let _ = framesync
            .receiver()
            .metadata()
            .try_capture(Duration::ZERO)?;
        let _ = framesync
            .receiver()
            .audio()
            .try_capture_ref(Duration::ZERO)?;
        let _ = framesync.receiver().poll_status_change(Duration::ZERO)?;
        receiver = framesync.into_receiver();
        assert!(receiver.video().try_capture(Duration::ZERO)?.is_none());
    }
    Ok(())
}
