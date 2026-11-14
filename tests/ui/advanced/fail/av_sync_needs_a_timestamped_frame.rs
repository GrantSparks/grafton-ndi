// AV sync matches audio to a frame's timestamp, so it is given a frame only
// as an AvSyncVideo, which a frame without a timestamp cannot become. Neither
// a frame itself nor the Option that AvSyncVideo::new returns is accepted, so
// a frame without a timestamp cannot silently become a request for all the
// queued audio either.
use grafton_ndi::{
    advanced::{AvSync, AvSyncCapture, AvSyncRequest, AvSyncVideo},
    VideoFrame,
};

fn built(sync: &AvSync, frame: &VideoFrame) {
    let _ = sync.synchronize(frame, AvSyncRequest::automatic());
}

fn unchecked(sync: &AvSync, frame: &VideoFrame) {
    let _ = sync.synchronize(AvSyncVideo::new(frame), AvSyncRequest::automatic());
}

fn optional(capture: &AvSyncCapture<'_>, frame: &VideoFrame) {
    let _ = capture.synchronize(Some(frame), AvSyncRequest::automatic());
}

fn main() {
    let _ = built;
    let _ = unchecked;
    let _ = optional;
}
