mod common;

use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverOptions, ScanType, Source, Tally,
};
use std::time::Duration;

// Raw audio capture on the attached receiver does not compile: the
// synchronizer takes the receiver's audio
// (tests/ui/fail/framesync_receiver_has_no_audio_capture.rs).
#[test]
fn attached_receiver_permits_controls_metadata_and_events_and_recovery_restores_raw_capture(
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
        let _ = framesync.receiver().poll_event(Duration::ZERO)?;
        receiver = framesync.into_receiver();
        assert!(receiver.video().capture(Duration::ZERO)?.is_none());
        assert!(receiver.audio().capture_ref(Duration::ZERO)?.is_none());
    }
    Ok(())
}
