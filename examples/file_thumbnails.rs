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
//! * This example requires exactly one video track. With program and proxy
//!   tracks, the 6.4 File player can report a `FirstFrame` timecode that does
//!   not arrive at the full-bandwidth receiver. Record with
//!   `FileRecorderOptions::proxy(false)`, as `file_record` does. A full-bandwidth
//!   receiver and [`Downscale::Quarter`] need no recorded proxy.
//! * After a seek the receiver first delivers the frames it had queued. The
//!   `FirstFrame` event carries the source timecode of the frame the seek
//!   landed on, so frames are discarded until one carries it. Video and
//!   events are drained together; playback pauses at that event while the
//!   receiver catches up and the image is encoded.
//!
//! The File library ships with the NDI SDK, beside the runtime library. On
//! Linux, macOS and Android it needs the Advanced SDK runtime.

use std::{
    env,
    num::NonZeroU32,
    time::{Duration, Instant},
};

use grafton_ndi::{
    file::{FileApi, FileApiOptions, FilePlayer, FilePlayerOptions, FilePosition, PlayerEventKind},
    image_encoding::{Downscale, ImageFormat, ImageOptions, ImageWorkspace},
    Finder, FinderOptions, Ndi, Receiver, ReceiverOptions, VideoFrameRef,
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
    let count = arguments.next().map_or(Ok(8), |count| count.parse())?;
    let count = NonZeroU32::new(count).ok_or("the count must be at least 1")?;

    let ndi = Ndi::new()?;
    let file = FileApi::new(&ndi, &FileApiOptions::default())?;
    let info = FilePlayer::probe(&file, &recording)?;
    if info.video_track_count() != 1 {
        return Err(
            "thumbnails require exactly one video track; record with FileRecorderOptions::proxy(false)"
                .into(),
        );
    }
    let length = info.duration().value();

    // A group of its own keeps the player's source off every other
    // receiver's list.
    let group = format!("thumbnails-{}", std::process::id());
    let mut player = FilePlayer::new(
        &file,
        &FilePlayerOptions::new("Thumbnails").groups([group.as_str()]),
    )?;
    let finder = Finder::new(
        &ndi,
        &FinderOptions::new()
            .groups([group])
            .extra_ips(["127.0.0.1"]),
    )?;
    let name = player.advertised_source()?.name().to_owned();
    let source = finder
        .find_source_by_name(&name, PATIENCE)?
        .ok_or_else(|| format!("the player's source {name} was not listed"))?;
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    let options = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);
    let mut workspace = ImageWorkspace::new();
    let mut encoded = Vec::new();

    // The initial frame can be sent before a new receiver has connected.
    // Establish the connection before starting the recording's timeline.
    let connected_by = Instant::now() + PATIENCE;
    while !receiver.is_connected() {
        if Instant::now() >= connected_by {
            return Err("the thumbnail receiver did not connect".into());
        }
        receiver.next_event(Duration::from_millis(50))?;
    }
    player.start(&recording)?;
    for i in 0..count.get() {
        let position = thumbnail_position(length, i, count);
        // Start already positioned the player at zero; reuse that first frame.
        if i != 0 {
            player.seek(position)?;
            player.set_paused(false);
        }
        let frame = first_frame(&mut player, &receiver)
            .map_err(|error| format!("thumbnail at {position:?}: {error}"))?;
        let path = format!("thumb_{i:03}.jpg");
        // Retain conversion scratch and output capacity between thumbnails.
        // Finish encoding before creating the output file.
        encoded.clear();
        workspace.encode_to(&frame, &options, &mut encoded)?;
        std::fs::write(&path, &encoded)?;
        println!(
            "{path}: {:.2} s, {}x{} reduced to a quarter",
            position.duration().as_secs_f64(),
            frame.width(),
            frame.height()
        );
    }
    Ok(())
}

fn thumbnail_position(length: FilePosition, index: u32, count: NonZeroU32) -> FilePosition {
    let ticks = u128::from(length.ticks()) * u128::from(index) / u128::from(count.get());
    // The loop always passes index < count, so the quotient fits in u64.
    FilePosition::from_ticks(ticks as u64)
}

#[cfg(test)]
mod position_tests {
    use super::*;

    #[test]
    fn positions_floor_to_ticks_and_stay_within_the_recording() {
        for (length, expected) in [
            (7, &[0][..]),
            (0, &[0, 0, 0]),
            (2, &[0, 0, 0, 1, 1]),
            (8, &[0, 2, 4, 6]),
            (9, &[0, 2, 4, 6]),
        ] {
            let length = FilePosition::from_ticks(length);
            let count = NonZeroU32::new(expected.len() as u32).unwrap();
            let positions: Vec<_> = (0..count.get())
                .map(|index| thumbnail_position(length, index, count).ticks())
                .collect();
            assert_eq!(positions, expected);
            assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
            if length.ticks() > 0 {
                assert!(positions.iter().all(|&position| position < length.ticks()));
            }
            for (index, &position) in (0..count.get()).zip(&positions) {
                let previous = length.duration() * index / count.get();
                assert_eq!(
                    FilePosition::from_duration(previous).unwrap().ticks(),
                    position
                );
            }
        }
    }

    #[test]
    fn positions_handle_maximum_duration_and_large_counts() {
        let length = FilePosition::from_ticks(u64::MAX);
        let count = NonZeroU32::new(10_000_002).unwrap();
        let last = thumbnail_position(length, count.get() - 1, count);
        assert_eq!(last.ticks(), 18_446_742_229_035_513_178);

        let count = NonZeroU32::new(u32::MAX).unwrap();
        assert_eq!(thumbnail_position(length, 0, count).ticks(), 0);
        let penultimate = thumbnail_position(length, count.get() - 2, count);
        let last = thumbnail_position(length, count.get() - 1, count);
        assert_eq!(last.ticks(), u64::MAX - (u64::from(u32::MAX) + 2));
        assert!(penultimate < last && last < length);
    }
}

/// Drain video while awaiting the player's event, then match its timecode.
/// Waiting only on events can fill the receive queue and lose the target frame.
fn first_frame<'a>(player: &mut FilePlayer, receiver: &'a Receiver) -> Result<VideoFrameRef<'a>> {
    let deadline = Instant::now() + PATIENCE;
    let mut first_frame = None;
    while Instant::now() < deadline {
        let frame = receiver.capture_video_ref(Duration::from_millis(10))?;
        // Read events after capture so a first-frame event queued during that
        // capture can identify this frame before it is released.
        while let Some(event) = player.next_event(Duration::ZERO)? {
            match event.kind() {
                PlayerEventKind::FirstFrame => {
                    // Keep playback from outrunning capture/encoding and
                    // discarding the seek target from the receiver's queue.
                    player.set_paused(true);
                    first_frame = Some(event);
                }
                PlayerEventKind::Error => {
                    return Err(
                        format!("playback failed: {}", event.message().to_string_lossy()).into(),
                    )
                }
                _ => {}
            }
        }
        if let (Some(frame), Some(event)) = (frame, first_frame.as_ref()) {
            // Without a preserved timecode, take the first available frame.
            if event
                .source_timecode()
                .is_none_or(|wanted| frame.timecode() == wanted)
            {
                return Ok(frame);
            }
        }
    }
    Err("the player's first frame was not received within ten seconds".into())
}
