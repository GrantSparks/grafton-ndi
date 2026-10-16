use grafton_ndi::{BorrowedVideoFrame, PixelFormat, Sender};

// Safe because the sender retains its own snapshot, independent of token Drop.
fn forgotten_send(sender: &mut Sender) {
    let buffer = vec![0u8; 16 * 16 * 4];
    let frame = BorrowedVideoFrame::try_from_uncompressed(
        &buffer, 16, 16, PixelFormat::BGRA, 30, 1,
    ).unwrap();
    std::mem::forget(sender.send_video_async(&frame));
    drop(frame);
    drop(buffer);
    sender.flush_async_blocking();
}

fn forgotten_sender(mut sender: Sender) {
    forgotten_send(&mut sender);
    // Leaking a sender retains SDK storage too; it cannot free a live buffer.
    std::mem::forget(sender);
}

fn main() {}
