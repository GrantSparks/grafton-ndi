//! Regressions for the runnable thumbnail example.
#![cfg(all(feature = "file-io", feature = "image-encoding"))]

#[path = "../examples/file_thumbnails.rs"]
#[allow(
    dead_code,
    reason = "the test calls the example after CLI argument parsing"
)]
mod example;

use example::{await_first_frame, Playback, EARLY_FRAME_LIMIT};
use grafton_ndi::file::FilePosition;
use std::{cell::Cell, time::Duration};

struct Frame<'a> {
    ticks: Option<i64>,
    pixel: u8,
    live: &'a Cell<usize>,
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}

fn seek(previous_end: u64) -> Playback {
    Playback::Seek {
        previous_end: FilePosition::from_ticks(previous_end),
    }
}

// Mock source momentarily holding a queue of complete borrowed frames. All
// frames have the same editorial timecode and distinct payloads.
#[derive(Clone, Copy)]
struct Step {
    frame: Option<(Option<i64>, u8)>,
    first_pts: Option<u64>,
}

fn step(frame: Option<(Option<i64>, u8)>, first_pts: Option<u64>) -> Step {
    Step { frame, first_pts }
}

fn scripted(playback: Playback, steps: &[Step]) -> (Result<(u8, u64), String>, usize) {
    let mut steps = steps.iter().copied();
    let event = Cell::new(None);
    let live = Cell::new(0);
    let peak = Cell::new(0);
    let result = await_first_frame(
        playback,
        Duration::from_millis(100),
        |remaining| {
            let Some(Step { frame, first_pts }) = steps.next() else {
                std::thread::sleep(remaining);
                return Ok(None);
            };
            event.set(first_pts.map(FilePosition::from_ticks));
            Ok(frame.map(|(ticks, pixel)| {
                live.set(live.get() + 1);
                peak.set(peak.get().max(live.get()));
                Frame {
                    ticks,
                    pixel,
                    live: &live,
                }
            }))
        },
        |_| Ok(event.take()),
        |frame| frame.ticks,
    )
    .map(|(frame, pts)| {
        assert_eq!(live.get(), 1, "only the selected guard remains borrowed");
        (frame.pixel, pts.ticks())
    })
    .map_err(|error| error.to_string());
    assert_eq!(live.get(), 0);
    (result, peak.get())
}

#[test]
fn chooses_the_playback_frame_on_either_side_of_the_event() {
    let (dark, bright, other) = (
        Some((Some(70), 0)),
        Some((Some(120), 230)),
        Some((Some(150), 100)),
    );
    for steps in [
        &[step(None, Some(120)), step(dark, None), step(bright, None)][..],
        &[step(bright, None), step(other, Some(120))],
        &[step(dark, None), step(bright, Some(120))],
        &[step(dark, None), step(bright, None), step(None, Some(120))],
    ] {
        assert_eq!(scripted(seek(80), steps).0.unwrap(), (230, 120));
    }
}

#[test]
fn zero_pts_can_only_identify_the_start_of_fresh_playback() {
    let first = Some((Some(2_000_000), 30));
    let next = Some((Some(1_000_000), 70));
    for steps in [
        [step(first, None), step(next, Some(0))],
        [step(None, Some(0)), step(first, None)],
    ] {
        assert_eq!(scripted(Playback::Start, &steps).0.unwrap(), (30, 0));
    }
    let error = scripted(seek(0), &[step(first, Some(0))]).0.unwrap_err();
    assert!(error.contains("zero PTS"), "{error}");

    for ticks in [None, Some(-1)] {
        let steps = [step(Some((ticks, 30)), None), step(next, Some(0))];
        let error = scripted(Playback::Start, &steps).0.unwrap_err();
        assert!(
            error.contains("opening frame has no usable receive timestamp"),
            "{error}"
        );
    }
}

#[test]
fn unusable_receive_timestamps_never_select_a_frame() {
    let steps = [
        step(Some((None, 0)), Some(120)),
        step(Some((Some(-1), 230)), None),
    ];
    let error = scripted(seek(80), &steps).0.unwrap_err();
    assert!(error.contains("no usable receive timestamp"), "{error}");

    let steps = [step(Some((Some(150), 230)), Some(120))];
    let error = scripted(seek(80), &steps).0.unwrap_err();
    assert!(
        error.contains("no received frame timestamp matched"),
        "{error}"
    );
}

#[test]
fn overlapping_seeks_and_duplicate_pts_are_refused() {
    for previous_end in [120, 130] {
        let steps = [step(Some((Some(120), 0)), Some(120))];
        let error = scripted(seek(previous_end), &steps).0.unwrap_err();
        assert!(
            error.contains("queued frames cannot be disambiguated"),
            "{error}"
        );
    }

    let steps = [
        step(Some((Some(120), 0)), None),
        step(Some((Some(120), 230)), Some(120)),
    ];
    let error = scripted(seek(80), &steps).0.unwrap_err();
    assert!(error.contains("multiple received frames"), "{error}");
}

#[test]
fn candidate_budget_releases_every_guard_on_success_and_exhaustion() {
    let mut steps: Vec<_> = (0..=EARLY_FRAME_LIMIT)
        .map(|index| step(Some((Some(120 + index as i64), index as u8)), None))
        .collect();
    let (result, peak) = scripted(seek(80), &steps);
    let error = result.unwrap_err();
    assert!(
        error.contains("candidate frames arrived before FirstFrame"),
        "{error}"
    );
    assert_eq!(
        peak,
        EARLY_FRAME_LIMIT + 1,
        "eight candidates plus the in-flight capture"
    );

    steps[EARLY_FRAME_LIMIT].first_pts = Some(120);
    let (result, peak) = scripted(seek(80), &steps);
    assert_eq!(result.unwrap(), (0, 120));
    assert_eq!(peak, EARLY_FRAME_LIMIT + 1);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
common_helpers!(runtime, file, fixture, name, loopback, unsupported);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn thumbnails_capture_dark_then_bright_with_the_same_editorial_timecode() {
    use grafton_ndi::{
        file::{
            FileNaming, FilePlayer, FileRecorder, FileRecorderOptions, FinishStatus,
            RecorderEventKind, RecordingTarget, RecordingTimecode, SessionEnd, SourceSpec,
        },
        image_encoding::{Downscale, ImageFormat, ImageOptions},
        MediaTypes, PixelFormat, SdkEdition, Sender, SenderOptions, Timecode, VideoFrame,
    };
    use std::{num::NonZeroU32, time::Instant};

    let Some((ndi, api)) = common::file_api() else {
        return;
    };
    let runtime = ndi.info();
    if runtime.edition() != SdkEdition::Advanced
        || !runtime.version().is_some_and(|version| {
            (
                version.major(),
                version.minor(),
                version.patch(),
                version.build(),
            ) == (6, 4, 0, 2)
        })
    {
        common::unsupported(&format!(
            "the exact PTS fixture covers Linux x86-64 Advanced 6.4.0.2, found {runtime:?}"
        ));
        return;
    }

    let fixture = common::FixtureDir::create("thumbnail-pts");
    let recording = fixture.join("recording.mkv");
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("thumbnail regression")),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let options = FileRecorderOptions::new(SourceSpec::try_from(&source).unwrap())
        .naming(FileNaming::Exact)
        .proxy(false)
        .tracks(MediaTypes::VIDEO);
    let mut recorder = FileRecorder::new(&api, &options).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&recording).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    let gray = |value| {
        VideoFrame::builder()
            .resolution(320, 180)
            .pixel_format(PixelFormat::Bgra)
            .timecode(Timecode::from_ticks(20))
            .data([value, value, value, 255].repeat(320 * 180))
            .build()
            .unwrap()
    };
    let (dark, bright) = (gray(0), gray(230));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "the recorder did not start");
        sender.send_video(&dark).unwrap();
        if let Some(event) = recorder.next_event(Duration::ZERO).unwrap() {
            assert!(
                !matches!(
                    event.kind(),
                    RecorderEventKind::Error | RecorderEventKind::OpenFailed
                ),
                "{event:?}"
            );
            if event.kind() == RecorderEventKind::FirstFrame {
                break;
            }
        }
    }
    // The clocked sender paces the frames. The midpoint of the recording
    // falls well into the bright segment.
    for _ in 0..24 {
        sender.send_video(&dark).unwrap();
    }
    for _ in 0..96 {
        sender.send_video(&bright).unwrap();
    }
    assert!(matches!(
        recorder.finish(Duration::from_secs(15)).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
    let info = FilePlayer::probe(&api, &recording).unwrap();
    assert_eq!(info.video_track_count(), 1);

    example::write_thumbnails(
        &ndi,
        &api,
        &recording,
        NonZeroU32::new(2).unwrap(),
        fixture.path(),
    )
    .unwrap();
    let assert_jpeg_gray = |jpeg: &[u8], expected: u8| {
        let mut decoder = jpeg_decoder::Decoder::new(jpeg);
        let pixels = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (80, 45));
        assert!(pixels.iter().all(|pixel| pixel.abs_diff(expected) <= 20));
    };
    assert_jpeg_gray(&std::fs::read(fixture.join("thumb_000.jpg")).unwrap(), 0);
    assert_jpeg_gray(&std::fs::read(fixture.join("thumb_001.jpg")).unwrap(), 230);

    // Establish a positive dark position and leave older dark video queued
    // across a seek into the bright segment. Both carry editorial timecode 20.
    let (mut player, receiver) =
        example::connected_player(&ndi, &api, "Thumbnail stale queue").unwrap();
    player.start(&recording).unwrap();
    let (_, opening_pts) = example::first_frame(&mut player, &receiver, Playback::Start).unwrap();
    let position_before_dark_seek = player.position().unwrap().max(opening_pts);
    player
        .seek(FilePosition::from_duration(Duration::from_millis(180)).unwrap())
        .unwrap();
    player.set_paused(false);
    let (_, dark_pts) = example::first_frame(
        &mut player,
        &receiver,
        seek(position_before_dark_seek.ticks()),
    )
    .unwrap();
    player.set_paused(false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while receiver.connection_stats().unwrap().video_frames_queued() < 2 {
        assert!(
            Instant::now() < deadline,
            "playback did not queue two frames"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    player.set_paused(true);
    let previous_end = player.position().unwrap().max(dark_pts);
    let midpoint = FilePosition::from_ticks(info.duration().value().ticks() / 2);
    player.seek(midpoint).unwrap();
    let stale = receiver
        .capture_video_ref(Duration::ZERO)
        .unwrap()
        .expect("queued dark frame after seek");
    let stale_ticks = u64::try_from(stale.timestamp().unwrap().ticks()).unwrap();
    assert!(
        (1..=previous_end.ticks()).contains(&stale_ticks),
        "paused end {previous_end:?}, stale timestamp {stale_ticks}"
    );
    assert_eq!(stale.timecode(), Timecode::from_ticks(20));
    let jpeg = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);
    assert_jpeg_gray(&stale.encode_image(&jpeg).unwrap(), 0);
    drop(stale);
    assert!(receiver.connection_stats().unwrap().video_frames_queued() >= 1);
    player.set_paused(false);
    let (fresh, fresh_pts) =
        example::first_frame(&mut player, &receiver, seek(previous_end.ticks())).unwrap();
    assert_eq!(fresh.timecode(), Timecode::from_ticks(20));
    assert_eq!(
        u64::try_from(fresh.timestamp().unwrap().ticks()).unwrap(),
        fresh_pts.ticks()
    );
    assert_jpeg_gray(&fresh.encode_image(&jpeg).unwrap(), 230);
    drop(fresh);

    let repeated = fixture.join("repeated");
    std::fs::create_dir(&repeated).unwrap();
    let (player, receiver) = example::connected_player(&ndi, &api, "Thumbnail repeat").unwrap();
    let error = example::write_positions(
        player,
        &receiver,
        &recording,
        [FilePosition::default(), midpoint, midpoint],
        &repeated,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("queued frames cannot be disambiguated"),
        "{error}"
    );
    assert!(repeated.join("thumb_000.jpg").is_file());
    assert!(repeated.join("thumb_001.jpg").is_file());
    assert!(!repeated.join("thumb_002.jpg").exists());
}
