#![allow(unused)]

use std::ffi::CString;

use grafton_ndi::{
    BorrowedVideoFrame, LineStrideOrSize, NDI, PixelFormat, ScanType, Sender, SenderOptions,
};

fn main() {
    let ndi = NDI::new().unwrap();
    let options = SenderOptions::builder("compile-contract").build();
    let mut sender = Sender::new(&ndi, &options).unwrap();
    let buffer = vec![0u8; 16 * 16 * 4];

    let token = {
        let metadata = CString::new("compile contract metadata").unwrap();
        let frame = unsafe {
            BorrowedVideoFrame::from_parts_unchecked(
                &buffer,
                16,
                16,
                PixelFormat::BGRA.into(),
                30,
                1,
                16.0 / 9.0,
                ScanType::Progressive,
                0,
                LineStrideOrSize::LineStrideBytes(16 * 4),
                Some(&metadata),
                0,
            )
        };

        sender.send_video_async(&frame)
    };

    drop(token);
}
