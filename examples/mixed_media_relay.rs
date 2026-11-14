//! Relay whichever decoded media arrives next with one blocking wait.
//!
//! Run: cargo run --example mixed_media_relay -- "HOST (Camera)" "Relay"
//! Input is the complete source name; stop with Ctrl-C. Synchronous media
//! sends borrow their capture guard, with reusable video staging where the
//! SDK requires initialized padding/tail. Metadata is forwarded from its borrow.
use grafton_ndi::{
    Error, Ndi, Receiver, ReceiverItemRef, ReceiverOptions, Sender, SenderOptions, Source,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

fn main() -> grafton_ndi::Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 || arguments.iter().any(String::is_empty) {
        return Err(Error::InvalidInput(
            "usage: mixed_media_relay \"HOST (Camera)\" \"Relay\"".into(),
        ));
    }
    let ndi = Ndi::new()?;
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(Source::new(arguments[0].clone())),
    )?;
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(&arguments[1])
            .clock_video(false)
            .clock_audio(false),
    )?;
    let running = Arc::new(AtomicBool::new(true));
    let stop = Arc::clone(&running);
    ctrlc::set_handler(move || stop.store(false, Ordering::Relaxed))
        .map_err(|error| Error::InvalidInput(error.to_string()))?;
    while running.load(Ordering::Relaxed) {
        match receiver.capture_any_ref(Duration::from_millis(100))? {
            Some(ReceiverItemRef::Video(frame)) => sender.send_video(&frame)?,
            Some(ReceiverItemRef::Audio(frame)) => sender.send_audio(&frame),
            Some(ReceiverItemRef::Metadata(frame)) => sender.send_metadata(&frame),
            Some(ReceiverItemRef::Event(event)) => eprintln!("receiver event: {event:?}"),
            _ => {}
        }
    }
    Ok(())
}
