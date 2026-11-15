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
async fn discover_and_analyze<R: grafton_ndi::asynchronous::AsyncRuntime>(
    ndi: &grafton_ndi::Ndi,
    name: &str,
) -> grafton_ndi::Result<()> {
    use grafton_ndi::{
        asynchronous::{AsyncFinder, AsyncReceiver},
        Finder, FinderOptions, Receiver, ReceiverOptions,
    };
    let finder = AsyncFinder::<R>::new(Finder::new(ndi, &FinderOptions::new())?);
    let Some(source) = finder
        .find_source_by_name(name, std::time::Duration::from_secs(5))
        .await?
    else {
        eprintln!("No source named {name:?} was found within five seconds.");
        return Ok(());
    };
    let receiver = Receiver::new(ndi, &ReceiverOptions::new(source))?;
    analyze(AsyncReceiver::<R>::new(receiver)).await
}

#[cfg(any(feature = "tokio", feature = "smol"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use grafton_ndi::Ndi;
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 1 || arguments[0].is_empty() {
        return Err("usage: mixed_media_async \"HOST (Camera)\"".into());
    }
    let ndi = Ndi::new()?;
    #[cfg(feature = "tokio")]
    {
        let runtime = ::tokio::runtime::Builder::new_current_thread().build()?;
        Ok(
            runtime.block_on(discover_and_analyze::<grafton_ndi::asynchronous::Tokio>(
                &ndi,
                &arguments[0],
            ))?,
        )
    }
    #[cfg(all(feature = "smol", not(feature = "tokio")))]
    Ok(::smol::block_on(discover_and_analyze::<
        grafton_ndi::asynchronous::Smol,
    >(&ndi, &arguments[0]))?)
}

#[cfg(not(any(feature = "tokio", feature = "smol")))]
fn main() {
    eprintln!("Enable the tokio or smol feature to run mixed_media_async.");
}
