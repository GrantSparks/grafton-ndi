#![allow(unused)]

use grafton_ndi::{BorrowedVideoFrame, NDI, PixelFormat, Sender, SenderOptions};

fn main() {
    let ndi = NDI::new().unwrap();
    let options = SenderOptions::builder("compile-contract").build();
    let mut sender = Sender::new(&ndi, &options).unwrap();

    let buffer_a = vec![0u8; 16 * 16 * 4];
    let frame_a =
        BorrowedVideoFrame::try_from_uncompressed(&buffer_a, 16, 16, PixelFormat::BGRA, 30, 1)
            .unwrap();
    let token_a = sender.send_video_async(&frame_a);

    let buffer_b = vec![1u8; 16 * 16 * 4];
    let frame_b =
        BorrowedVideoFrame::try_from_uncompressed(&buffer_b, 16, 16, PixelFormat::BGRA, 30, 1)
            .unwrap();
    let token_b = sender.send_video_async(&frame_b);

    drop((token_a, token_b));
}
