//! Inspect borrowed mixed media on a blocking worker and return small owned summaries.
//!
//! Run: cargo run --features tokio --example mixed_media_async -- "HOST (Camera)"
//! Or: cargo run --features smol --example mixed_media_async -- "HOST (Camera)"
//! Stops after 100 items or ten consecutive one-second timeouts.

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn analyze<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: grafton_ndi::asynchronous::AsyncReceiver<R>,
) -> grafton_ndi::Result<()> {
    use grafton_ndi::{ReceiverEvent, ReceiverItemRef, Timecode};
    use std::time::Duration;
    enum Summary {
        Video(u32, u32, Timecode),
        Audio(u32, u32),
        Metadata(usize),
        Event(ReceiverEvent),
        Other,
    }
    let mut received = 0;
    let mut silent = 0;
    while received < 100 && silent < 10 {
        // The callback retains the SDK buffer only during this bounded,
        // synchronous inspection. Only the summary crosses the task boundary.
        let Some(item) = receiver
            .capture_any_with(Duration::from_secs(1), |item| match item {
                ReceiverItemRef::Video(frame) => {
                    Summary::Video(frame.width(), frame.height(), frame.timecode())
                }
                ReceiverItemRef::Audio(frame) => {
                    Summary::Audio(frame.num_channels(), frame.num_samples())
                }
                ReceiverItemRef::Metadata(frame) => Summary::Metadata(frame.data().len()),
                ReceiverItemRef::Event(event) => Summary::Event(*event),
                _ => Summary::Other,
            })
            .await?
        else {
            silent += 1;
            continue;
        };
        silent = 0;
        received += 1;
        match item {
            Summary::Video(width, height, timecode) => {
                println!("video {width}x{height} at {timecode:?}")
            }
            Summary::Audio(channels, samples) => {
                println!("audio {channels} channels, {samples} samples/channel")
            }
            Summary::Metadata(bytes) => println!("metadata {bytes} bytes"),
            Summary::Event(event) => println!("event {event:?}"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(any(feature = "tokio", feature = "smol"))]
fn main() -> grafton_ndi::Result<()> {
    use grafton_ndi::{Error, Ndi, Receiver, ReceiverOptions, Source};
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 1 || arguments[0].is_empty() {
        return Err(Error::InvalidInput(
            "usage: mixed_media_async \"HOST (Camera)\"".into(),
        ));
    }
    let ndi = Ndi::new()?;
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(Source::new(arguments[0].clone())),
    )?;
    #[cfg(feature = "tokio")]
    {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| Error::SpawnFailed(error.to_string()))?;
        runtime.block_on(analyze(
            grafton_ndi::asynchronous::tokio::AsyncReceiver::new(receiver),
        ))
    }
    #[cfg(all(feature = "smol", not(feature = "tokio")))]
    ::smol::block_on(analyze(
        grafton_ndi::asynchronous::smol::AsyncReceiver::new(receiver),
    ))
}

#[cfg(not(any(feature = "tokio", feature = "smol")))]
fn main() {
    eprintln!("Enable the tokio or smol feature to run mixed_media_async.");
}
