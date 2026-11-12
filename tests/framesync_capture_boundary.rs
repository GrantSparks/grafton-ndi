mod common;

use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, ScanType, Source, Tally,
};
use std::time::Duration;

#[test]
fn attached_receiver_permits_controls_audio_metadata_and_raw_capture_after_recovery(
) -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let source = Source::new("FrameSync capture boundary");
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    for _ in 0..3 {
        let framesync = FrameSync::new(receiver)?;
        let _video = framesync.capture_video_ref(ScanType::Progressive)?;
        drop(_video);
        let _audio =
            framesync.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 32)?)?;
        drop(_audio);
        framesync.receiver().set_tally(Tally::new(false, true));
        assert!(!framesync.receiver().ptz().is_supported());
        let _ = framesync.receiver().metadata().capture(Duration::ZERO)?;
        let _ = framesync.receiver().audio().capture_ref(Duration::ZERO)?;
        let _ = framesync.receiver().poll_event(Duration::ZERO)?;
        receiver = framesync.into_receiver();
        assert!(receiver.video().capture(Duration::ZERO)?.is_none());
    }
    Ok(())
}
