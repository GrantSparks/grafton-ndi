use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender, VideoFrame};

// A synchronous send lends a borrowed (here padded) frame without copying it,
// and the SDK has released the buffer when `send_video` returns.
fn borrowed(sender: &Sender, buffer: &mut Vec<u8>) {
    let frame = BorrowedVideoFrame::builder(buffer, 64, 32, PixelFormat::Bgra)
        .line_stride(512)
        .build()
        .unwrap();
    sender.send_video(&frame).unwrap();
    buffer.fill(0);
}

// The frame may also be sent by reference, and reused for a second send.
fn borrowed_by_reference(sender: &Sender, buffer: &mut [u8]) {
    let frame = BorrowedVideoFrame::builder(buffer, 64, 32, PixelFormat::Bgra)
        .build()
        .unwrap();
    sender.send_video(&frame).unwrap();
    sender.send_video(&frame).unwrap();
    buffer[0] = 1;
}

// An owned frame is still sent by reference.
fn owned(sender: &Sender, frame: &mut VideoFrame) {
    sender.send_video(&*frame).unwrap();
    frame.data_mut().fill(0);
}

fn main() {}
