//! Example: thumbnails of a recording, where the File API and image encoding
//! meet.
//!
//! Plays a recording into a private NDI group, seeks to evenly spaced
//! positions, receives the frame shown at each and writes it as a
//! quarter-size JPEG: `thumb_000.jpg`, `thumb_001.jpg` and so on, in the
//! current directory.
//!
//! Run with:
//! `cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8`
//!
//! Arguments: the recording, and the number of thumbnails (8 by default).
//!
//! It stays an example rather than an API, because it advertises an NDI
//! source while it runs, and each choice below rests on how the 6.4 Advanced
//! runtime and File library behave with a SpeedHQ recording:
//!
//! * A receiver cannot connect to `player.advertised_source()` as it is: it
//!   has no address. A [`Finder`] in the player's groups lists the source
//!   with one.
//! * A `Lowest`-bandwidth receiver, as `ReceiverOptions::snapshot_preset`
//!   makes, gets video only from a recording with a proxy track. A
//!   full-bandwidth receiver and [`Downscale::Quarter`] work for every
//!   recording.
//! * After a seek the receiver first delivers the frames it had queued. The
//!   `FirstFrame` event carries the source timecode of the frame the seek
//!   landed on, so frames are discarded until one carries it.
//!
//! The File library ships with the NDI SDK, beside the runtime library. On
//! Linux, macOS and Android it needs the Advanced SDK runtime.

use std::{
    env,
    time::{Duration, Instant},
};

use grafton_ndi::{
    file::{FileApi, FilePlayer, FilePosition, FileRuntimeOptions, PlayerEventKind, PlayerOptions},
    Downscale, Finder, FinderOptions, ImageFormat, ImageOptions, Ndi, Receiver, ReceiverOptions,
    Source, Timecode,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// How long each step may take before the example gives up.
const PATIENCE: Duration = Duration::from_secs(10);

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let Some(recording) = arguments.next() else {
        eprintln!("usage: file_thumbnails <recording.mkv> [count]");
        std::process::exit(2);
    };
    let count: u32 = arguments.next().map_or(Ok(8), |count| count.parse())?;
    if count == 0 {
        return Err("the count must be at least 1".into());
    }

    let ndi = Ndi::new()?;
    let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
    let length = file.probe(&recording)?.duration().value().duration();

    // A group of its own keeps the player's source off every other
    // receiver's list.
    let group = format!("thumbnails-{}", std::process::id());
    let mut player =
        file.create_player(&PlayerOptions::new("Thumbnails").groups(group.as_str()))?;
    let finder = Finder::new(&ndi, &FinderOptions::new().groups(group))?;
    let source = wait_for_source(&finder, player.advertised_source()?.name())?;
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    let options = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);

    player.start(&recording)?;
    first_frame_timecode(&mut player)?;
    for i in 0..count {
        let position = length * i / count;
        player.seek(FilePosition::from_duration(position)?)?;
        let wanted = first_frame_timecode(&mut player)?;
        let deadline = Instant::now() + PATIENCE;
        let frame = loop {
            if Instant::now() > deadline {
                return Err(format!("no frame of the seek to {position:?} arrived").into());
            }
            let Some(frame) = receiver.video().capture_ref(Duration::from_secs(1))? else {
                continue;
            };
            // Without a timecode the SDK cannot say which frame is the
            // target, and the first one to arrive is taken.
            if wanted.is_none_or(|wanted| frame.timecode() == wanted) {
                break frame;
            }
        };
        let path = format!("thumb_{i:03}.jpg");
        std::fs::write(&path, frame.encode_image(&options)?)?;
        println!(
            "{path}: {:.2} s, {}x{} reduced to a quarter",
            position.as_secs_f64(),
            frame.width(),
            frame.height()
        );
    }
    Ok(())
}

/// The source this process advertises as `name`, once `finder` lists it.
fn wait_for_source(finder: &Finder, name: &str) -> Result<Source> {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Some(source) = finder
            .current_sources()?
            .into_iter()
            .find(|source| source.name() == name)
        {
            return Ok(source);
        }
        finder.wait_for_sources(Duration::from_millis(100))?;
    }
    Err(format!("the player's source {name} was not listed").into())
}

/// Wait for the player's next `FirstFrame` event, and return the source
/// timecode of the frame it shows, when the SDK knows it.
fn first_frame_timecode(player: &mut FilePlayer) -> Result<Option<Timecode>> {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Some(event) = player.poll_event(Duration::from_millis(100))? {
            match event.kind() {
                PlayerEventKind::FirstFrame => return Ok(event.source_timecode()),
                PlayerEventKind::Error => {
                    return Err(
                        format!("playback failed: {}", event.message().to_string_lossy()).into(),
                    )
                }
                _ => {}
            }
        }
    }
    Err("the player reported no first frame".into())
}
