#![allow(unused)]
use grafton_ndi::{Sender, VideoFrame};
fn resend(sender: &mut Sender, frame: VideoFrame) {
    sender.send_video_async(frame);
    sender.send_video_async(frame);
}
fn main() {}
