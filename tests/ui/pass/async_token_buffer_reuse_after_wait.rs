#![allow(unused)]

use grafton_ndi::{BorrowedVideoFrame, NDI, PixelFormat, Sender, SenderOptions};

fn main() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let options = SenderOptions::builder("compile-contract").build();
    let mut sender = Sender::new(&ndi, &options)?;

    let mut buffer = vec![0u8; 16 * 16 * 4];
    let token = {
        let frame =
            BorrowedVideoFrame::try_from_uncompressed(&buffer, 16, 16, PixelFormat::BGRA, 30, 1)?;
        sender.send_video_async(&frame)
    };

    token.wait()?;
    buffer[0] = 1;

    Ok(())
}
