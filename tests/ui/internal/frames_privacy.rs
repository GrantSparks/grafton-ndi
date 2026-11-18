//! Even safe code in frames cannot construct any invariant-carrying view.
#![deny(unsafe_code)]
use super::*;

fn video_layout(value: ValidatedVideoLayout) {
    let _ = ValidatedVideoLayout { ..value };
}
fn audio_layout(value: ValidatedAudioLayout) {
    let _ = ValidatedAudioLayout { ..value };
}
fn video_plane(value: VideoPlane<'_>) {
    let _ = VideoPlane { ..value };
}
fn video_planes(value: VideoPlanes<'_>) {
    let _ = VideoPlanes { ..value };
}
fn video_rows(value: VideoRows<'_>) {
    let _ = VideoRows { ..value };
}
fn audio_channels(value: AudioChannels<'_>) {
    let _ = AudioChannels { ..value };
}
fn validated_frame<F, V>(value: ValidatedFrame<F, V>) {
    let _ = ValidatedFrame { ..value };
}
fn packed_video(value: PackedVideo<'_>) {
    let _ = PackedVideo { ..value };
}
fn borrowed_video(value: crate::BorrowedVideoFrame<'_>) {
    let _ = crate::BorrowedVideoFrame { ..value };
}
fn borrowed_audio(value: BorrowedAudioFrame<'_>) {
    let _ = BorrowedAudioFrame { ..value };
}
