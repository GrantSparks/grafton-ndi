use grafton_ndi::{
    BorrowedVideoFrame, Error, FrameSync, FrameSyncAttached, FrameSyncAudioRef,
    FrameSyncAudioRequest, Receiver, ReceiverOptions, Result, Sender, Source,
    SourceAddress, Tally, VideoFrame, NDI,
};

fn send_sync<T: Send + Sync>() {}

// Compile the 2.0 signatures and public fields used around the touched code.
fn old_calls(sender: &Sender, receiver: &Receiver, fs: &FrameSync) -> Result<()> {
    let _: fn(&Sender, &VideoFrame) = Sender::send_video;
    let _: fn() -> Result<String> = NDI::version;
    let _: fn(i32, i32) -> Result<FrameSyncAudioRequest> = FrameSyncAudioRequest::capture;
    let _: Option<Tally> = sender.tally(std::time::Duration::ZERO)?;
    let _: VideoFrame = receiver.video().capture(std::time::Duration::ZERO)?;
    let _: Option<VideoFrame> = receiver.video().try_capture(std::time::Duration::ZERO)?;
    let _: FrameSyncAudioRef<'_> = fs.capture_audio(FrameSyncAudioRequest::query_input())?;
    let _: Option<_> = fs.receiver().audio().try_capture(std::time::Duration::ZERO)?;
    let _: ReceiverOptions = ReceiverOptions::default();
    let _ = Source { name: String::new(), address: SourceAddress::None };
    Ok(())
}

fn main() {
    send_sync::<NDI>();
    send_sync::<Sender>();
    send_sync::<Receiver>();
    send_sync::<Receiver<FrameSyncAttached>>();
    send_sync::<FrameSync>();
    send_sync::<VideoFrame>();
    send_sync::<BorrowedVideoFrame<'static>>();
    send_sync::<Error>();
    let _ = old_calls;
}
