use grafton_ndi::{
    advanced::{AllocatedReceiver, AvSync, AvSyncRequest, MouseButton},
    FrameSync, FrameSyncAudioRequest, MediaTypes, Result, ScanType, StreamTier, Tally,
};

fn framesync(sync: &mut FrameSync) -> Result<()> {
    sync.controls().set_stream_tier(StreamTier::Preview)?;
    sync.controls().set_streams(MediaTypes::ALL)?;
    let (capture, controls) = sync.split();
    controls.receiver().set_tally(Tally::new(false, true));
    let mut session = controls.kvm()?;
    session.mouse_down(MouseButton::Left)?;
    let video = capture.capture_video_ref(ScanType::Progressive)?;
    let audio = capture.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 480)?)?;
    session.key_down(0xffe1)?;
    let _ = capture.capture_video(ScanType::Progressive)?;
    let _ = capture.audio_input_format()?;
    let _ = capture.audio_queue_depth();
    session.release_all()?;
    drop((video, audio));
    Ok(())
}

fn avsync(sync: &mut AvSync) -> Result<()> {
    let (capture, controls) = sync.split();
    let mut session = controls.kvm()?;
    session.mouse_down(MouseButton::Left)?;
    let video = session
        .receiver()
        .video()
        .capture_ref(std::time::Duration::ZERO)?;
    if let Some(video) = &video {
        let _ = capture.synchronize(Some(video), AvSyncRequest::automatic())?;
    }
    session.release_all()?;
    Ok(())
}

fn allocated(receiver: &mut AllocatedReceiver) -> Result<()> {
    receiver.controls().set_stream_tier(StreamTier::Program)?;
    receiver.controls().set_streams(MediaTypes::ALL)?;
    let mut session = receiver.controls().kvm()?;
    session.key_down(0xffe1)?;
    let _ = session
        .receiver()
        .capture_any_ref(std::time::Duration::ZERO)?;
    session.release_all()
}

fn main() {
    let _ = (framesync, avsync, allocated);
}
