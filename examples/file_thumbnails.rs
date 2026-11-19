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
//! source while it runs, and exact matching was verified with the 6.4.0.2
//! Advanced runtime and File library on Linux x86-64 with a SpeedHQ recording.
//!
//! * A receiver cannot connect to `player.advertised_source()` as it is: it
//!   has no address. A [`Finder`] in the player's groups lists the source
//!   with one.
//! * This example requires exactly one video track. With program and proxy
//!   tracks, the 6.4 File player can report a `FirstFrame` timestamp that does
//!   not arrive at the full-bandwidth receiver. Record with
//!   `FileRecorderOptions::proxy(false)`, as `file_record` does. A full-bandwidth
//!   receiver and [`Downscale::Quarter`] need no recorded proxy.
//! * Frames queued before a seek can arrive afterward. Match the `FirstFrame`
//!   event's file presentation timestamp (PTS) to the raw receive timestamp.
//!   The editorial timecode can repeat. Keep a bounded set of borrowed frames
//!   while awaiting the event and pause on it to limit further queuing.
//!   If the opening event reports zero, use the earliest captured video on the
//!   fresh receiver if it has a raw timestamp and the receiver reports no
//!   drops. A seek requires a nonzero PTS beyond the prior playback position.
//!
//! The File library ships with the NDI SDK, beside the runtime library. On
//! Linux, macOS and Android it needs the Advanced SDK runtime.

use std::{
    env,
    num::NonZeroU32,
    path::Path,
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
const CAPTURE_SLICE: Duration = Duration::from_millis(10);
/// Keep at most this many early SDK buffers across capture calls.
pub(crate) const EARLY_FRAME_LIMIT: usize = 8;

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
    write_thumbnails(&ndi, &file, Path::new(&recording), count, Path::new("."))
}

pub(crate) fn write_thumbnails(
    ndi: &Ndi,
    file: &FileApi,
    recording: &Path,
    count: NonZeroU32,
    output: &Path,
) -> Result<()> {
    let info = FilePlayer::probe(file, recording)?;
    if info.video_track_count() != 1 {
        return Err(
            "thumbnails require exactly one video track; record with FileRecorderOptions::proxy(false)"
                .into(),
        );
    }
    let length = info.duration().value();
    let positions = (0..count.get()).map(|index| thumbnail_position(length, index, count));
    let (player, receiver) = connected_player(ndi, file, "Thumbnails")?;
    write_positions(player, &receiver, recording, positions, output)
}

pub(crate) fn write_positions(
    mut player: FilePlayer,
    receiver: &Receiver,
    recording: &Path,
    positions: impl IntoIterator<Item = FilePosition>,
    output: &Path,
) -> Result<()> {
    let options = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);
    let mut workspace = ImageWorkspace::new();
    let mut encoded = Vec::new();

    player.start(recording)?;
    let mut previous_step = None;
    for (i, position) in positions.into_iter().enumerate() {
        let (frame, first_pts) = (|| {
            // Start positioned the player at zero. For subsequent seeks, use
            // its last sent position while still paused to bound stale media.
            let playback = match previous_step {
                None => Playback::Start,
                Some((previous_request, pts)) => {
                    if position <= previous_request {
                        return Err("the requested seek did not advance; queued frames cannot be disambiguated".into());
                    }
                    let last_sent = player.position().ok_or(
                        "the paused player's last position is unavailable; queued frames cannot be disambiguated",
                    )?;
                    player.seek(position)?;
                    player.set_paused(false);
                    Playback::after_pause(last_sent, pts)
                }
            };
            first_frame(&mut player, receiver, playback)
        })()
        .map_err(|error| format!("thumbnail at {position:?}: {error}"))?;
        previous_step = Some((position, first_pts));
        let path = format!("thumb_{i:03}.jpg");
        // Retain conversion scratch and output capacity between thumbnails.
        // Finish encoding before creating the output file.
        encoded.clear();
        workspace.encode_to(&frame, &options, &mut encoded)?;
        let (width, height) = (frame.width(), frame.height());
        drop(frame);
        std::fs::write(output.join(&path), &encoded)?;
        println!(
            "{path}: {:.2} s, {width}x{height} reduced to a quarter",
            position.duration().as_secs_f64()
        );
    }
    Ok(())
}

pub(crate) fn connected_player(
    ndi: &Ndi,
    file: &FileApi,
    player_name: &str,
) -> Result<(FilePlayer, Receiver)> {
    // A group of its own keeps the player's source off every other receiver's
    // list. Connect before starting playback so we can capture its first frame.
    let group = format!("{player_name}-{}", std::process::id());
    let player = FilePlayer::new(
        file,
        &FilePlayerOptions::new(player_name).groups([group.as_str()]),
    )?;
    let finder = Finder::new(
        ndi,
        &FinderOptions::new()
            .groups([group])
            .extra_ips(["127.0.0.1"]),
    )?;
    let name = player.advertised_source()?.name().to_owned();
    let source = finder
        .find_source_by_name(&name, PATIENCE)?
        .ok_or_else(|| format!("the player's source {name} was not listed"))?;
    let receiver = Receiver::new(ndi, &ReceiverOptions::new(source))?;
    let connected_by = Instant::now() + PATIENCE;
    while !receiver.is_connected() {
        if Instant::now() >= connected_by {
            return Err("the thumbnail receiver did not connect".into());
        }
        receiver.next_event(Duration::from_millis(50))?;
    }
    Ok((player, receiver))
}

fn thumbnail_position(length: FilePosition, index: u32, count: NonZeroU32) -> FilePosition {
    let ticks = u128::from(length.ticks()) * u128::from(index) / u128::from(count.get());
    // The loop always passes index < count, so the quotient fits in u64.
    FilePosition::from_ticks(ticks as u64)
}

#[derive(Clone, Copy)]
pub(crate) enum Playback {
    Start,
    Seek { previous_end: FilePosition },
}

impl Playback {
    pub(crate) fn after_pause(last_sent: FilePosition, first_pts: FilePosition) -> Self {
        // The verified File runtime can report position in whole milliseconds
        // while outgoing video timestamps have sub-millisecond precision.
        // Include the rest of that resolution before permitting another seek.
        let previous_end = FilePosition::from_ticks(last_sent.ticks().saturating_add(9_999));
        Self::Seek {
            previous_end: previous_end.max(first_pts),
        }
    }

    fn validate_pts(self, position: FilePosition) -> Result<()> {
        if let Self::Seek { previous_end } = self {
            if position.ticks() == 0 {
                return Err(
                    "the seek's FirstFrame has zero PTS; queued frames cannot be disambiguated"
                        .into(),
                );
            }
            if position <= previous_end {
                return Err(format!(
                    "the seek's FirstFrame PTS {} overlaps the previous playback through PTS {}; queued frames cannot be disambiguated",
                    position.ticks(), previous_end.ticks(),
                ).into());
            }
        }
        // The signed maximum is reserved by the receive timestamp field.
        if position.ticks() >= i64::MAX as u64 {
            return Err("the FirstFrame PTS exceeds the receive timestamp range".into());
        }
        Ok(())
    }
}

/// Drain video while awaiting the player's event, then match its file PTS.
/// Waiting only on events can fill the receive queue and lose the target frame.
pub(crate) fn first_frame<'a>(
    player: &mut FilePlayer,
    receiver: &'a Receiver,
    playback: Playback,
) -> Result<(VideoFrameRef<'a>, FilePosition)> {
    let (frame, position) = await_first_frame(
        playback,
        PATIENCE,
        |timeout| Ok(receiver.capture_video_ref(timeout)?),
        |deadline| {
            let mut position = None;
            while Instant::now() < deadline {
                let Some(event) = player.next_event(Duration::ZERO)? else {
                    break;
                };
                match event.kind() {
                    PlayerEventKind::FirstFrame => {
                        // Limit how far playback advances before encoding.
                        player.set_paused(true);
                        if position.replace(event.first_pts()).is_some() {
                            return Err(
                                "multiple FirstFrame events arrived for one playback step".into()
                            );
                        }
                    }
                    PlayerEventKind::Error => {
                        return Err(format!(
                            "playback failed: {}",
                            event.message().to_string_lossy()
                        )
                        .into());
                    }
                    _ => {}
                }
            }
            Ok(position)
        },
        |frame| frame.timestamp().map(|timestamp| timestamp.ticks()),
    )?;
    if position.ticks() == 0 && receiver.connection_stats()?.video_frames_dropped() != 0 {
        return Err("the receiver dropped video during zero-PTS startup".into());
    }
    Ok((frame, position))
}

pub(crate) fn await_first_frame<T>(
    playback: Playback,
    patience: Duration,
    mut capture: impl FnMut(Duration) -> Result<Option<T>>,
    mut poll_first_pts: impl FnMut(Instant) -> Result<Option<FilePosition>>,
    timestamp: impl Fn(&T) -> Option<i64>,
) -> Result<(T, FilePosition)> {
    let deadline = Instant::now() + patience;
    let mut early = Vec::new();
    let mut wanted: Option<FilePosition> = None;
    let mut saw_timestamp = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let candidate = capture(remaining.min(CAPTURE_SLICE))?.and_then(|frame| {
            let ticks = timestamp(&frame).and_then(|ticks| u64::try_from(ticks).ok());
            saw_timestamp |= ticks.is_some();
            if let Playback::Seek { previous_end } = playback {
                if ticks.is_none_or(|ticks| ticks <= previous_end.ticks()) {
                    return None;
                }
            }
            Some((ticks, frame))
        });

        // A frame can already be in hand when its event arrives. Check it
        // together with earlier captures and release everything else now.
        if let Some(position) = poll_first_pts(deadline)? {
            if wanted.replace(position).is_some() {
                return Err("multiple FirstFrame events arrived for one playback step".into());
            }
            playback.validate_pts(position)?;
            let mut frames = early.drain(..).chain(candidate);
            // Zero has no reliable raw timestamp on startup in the verified
            // runtime. There cannot be earlier media on this fresh receiver.
            if position.ticks() == 0 {
                if let Some(candidate) = frames.next() {
                    return Ok((opening_frame(candidate)?, position));
                }
                continue;
            }
            let mut matches = frames.filter(|(ticks, _)| *ticks == Some(position.ticks()));
            if let Some((_, frame)) = matches.next() {
                if matches.next().is_some() {
                    return Err("multiple received frames carry the FirstFrame PTS".into());
                }
                return Ok((frame, position));
            }
        } else if let Some((ticks, frame)) = candidate {
            match wanted {
                Some(position) if position.ticks() == 0 => {
                    return Ok((opening_frame((ticks, frame))?, position));
                }
                Some(position) if ticks == Some(position.ticks()) => {
                    return Ok((frame, position));
                }
                Some(_) => {}
                None if early.len() == EARLY_FRAME_LIMIT => {
                    return Err(format!(
                        "more than {EARLY_FRAME_LIMIT} candidate frames arrived before FirstFrame"
                    )
                    .into());
                }
                None => early.push((ticks, frame)),
            }
        }
    }

    match wanted {
        Some(position) if position.ticks() == 0 => {
            Err("no video frame arrived after startup within ten seconds".into())
        }
        Some(position) if saw_timestamp => Err(format!(
            "no received frame timestamp matched FirstFrame PTS {} within ten seconds",
            position.ticks()
        )
        .into()),
        Some(_) => Err("no usable receive timestamp arrived within ten seconds".into()),
        None => Err("the player did not report FirstFrame within ten seconds".into()),
    }
}

fn opening_frame<T>((ticks, frame): (Option<u64>, T)) -> Result<T> {
    ticks.ok_or("the opening frame has no usable receive timestamp")?;
    Ok(frame)
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
