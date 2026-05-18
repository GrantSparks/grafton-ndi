#![allow(unused)]

use grafton_ndi::{BorrowedVideoFrame, NDI, PixelFormat, Sender, SenderOptions};

fn main() {
    let ndi = NDI::new().unwrap();
    let options = SenderOptions::builder("compile-contract").build();
    let mut sender = Sender::new(&ndi, &options).unwrap();

    let buffer = vec![0u8; 16 * 16 * 4];
    let frame =
        BorrowedVideoFrame::try_from_uncompressed(&buffer, 16, 16, PixelFormat::BGRA, 30, 1)
            .unwrap();
    let token = sender.send_video_async(&frame);

    drop(buffer);
    drop(token);
}
