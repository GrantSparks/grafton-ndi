//! The File API against a real File library.
//!
//! Every test starts with [`file_api`], which follows the crate's test policy:
//! it runs wherever an NDI runtime and its File library are installed, is
//! visibly skipped elsewhere, and fails instead when
//! `GRAFTON_NDI_REQUIRE_RUNTIME` promises `file`. Each test records its own
//! material from a sender on the loopback interface; none needs a prepared file.

mod async_shutdown;

use super::*;
use crate::test_support::FixtureDir;
use crate::{
    test_support::{self, file_api},
    MediaTypes,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The longest any single SDK step is given before a test fails.
const PATIENCE: Duration = Duration::from_secs(15);

/// A sender that produces video until dropped, reachable over loopback.
pub(super) struct LiveSource {
    spec: SourceSpec,
    running: Arc<AtomicBool>,
    larger_frames: Arc<AtomicBool>,
    producer: Option<JoinHandle<()>>,
}

impl LiveSource {
    pub(super) fn start(ndi: &crate::Ndi, test: &str) -> Self {
        let name = format!("grafton-file-{}-{test}", std::process::id());
        let sender = crate::Sender::new(ndi, &crate::SenderOptions::new(name.as_str())).unwrap();
        let spec = Self::loopback(ndi, &sender);
        let running = Arc::new(AtomicBool::new(true));
        let keep_running = Arc::clone(&running);
        let larger_frames = Arc::new(AtomicBool::new(false));
        let send_larger = Arc::clone(&larger_frames);
        let producer = thread::spawn(move || {
            let frame = |width, height| {
                crate::VideoFrame::builder()
                    .resolution(width, height)
                    .pixel_format(crate::PixelFormat::Bgra)
                    .build()
                    .unwrap()
            };
            let (small, large) = (frame(320, 180), frame(640, 360));
            while keep_running.load(Ordering::Acquire) {
                sender
                    .send_video(if send_larger.load(Ordering::Acquire) {
                        &large
                    } else {
                        &small
                    })
                    .unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            spec,
            running,
            larger_frames,
            producer: Some(producer),
        }
    }

    /// Switch the source to another resolution, which a recorder sees as a
    /// format change.
    fn change_format(&self) {
        self.larger_frames.store(true, Ordering::Release);
    }

    /// The sender's endpoint on the loopback interface, so that recording does
    /// not depend on which other interfaces the machine has.
    fn loopback(ndi: &crate::Ndi, sender: &crate::Sender) -> SourceSpec {
        let name = sender.source().unwrap().name().to_owned();
        let finder =
            crate::Finder::new(ndi, &crate::FinderOptions::new().extra_ips(["127.0.0.1"])).unwrap();
        let source = finder
            .find_source(PATIENCE, |source| {
                source.name() == name && source.address().is_some_and(|address| !address.is_url())
            })
            .unwrap()
            .unwrap_or_else(|| panic!("{name} was not discovered"));
        let port = source
            .address()
            .unwrap()
            .port()
            .expect("an address with a port");
        let selected =
            source.with_address(crate::SourceAddress::new(format!("127.0.0.1:{port}")).unwrap());
        SourceSpec::try_from(&selected).unwrap()
    }
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

/// Options that record the live source's video to exactly the given file name.
pub(super) fn exact_video(source: &LiveSource) -> FileRecorderOptions {
    FileRecorderOptions::new(source.spec.clone())
        .naming(FileNaming::Exact)
        .proxy(false)
        .tracks(MediaTypes::VIDEO)
}

/// Check that `track` describes the program video [`LiveSource`] sends
/// before its format changes: 320x180 progressive at 60 frames per second,
/// 16:9.
fn assert_live_video(track: &VideoTrack) {
    assert_eq!(track.tier(), crate::StreamTier::Program, "{track:?}");
    assert_eq!((track.width(), track.height()), (320, 180), "{track:?}");
    let rate = track.frame_rate().expect("a known frame rate");
    assert_eq!(rate.numerator(), 60 * rate.denominator(), "{rate:?}");
    assert_eq!(track.scan_type(), Some(crate::ScanType::Progressive));
    assert!(
        track
            .picture_aspect_ratio()
            .is_some_and(|ratio| (ratio.get() - 16.0 / 9.0).abs() < 1e-3),
        "{track:?}"
    );
}

/// Poll until an event of `kind` arrives, failing on a recorder error.
fn await_event(recorder: &mut FileRecorder, kind: RecorderEventKind) -> RecorderEvent {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let Some(event) = recorder.next_event(Duration::from_millis(50)).unwrap() else {
            continue;
        };
        assert!(
            !matches!(
                event.kind(),
                RecorderEventKind::Error | RecorderEventKind::OpenFailed
            ),
            "{:?}: {}",
            event.kind(),
            event.message().to_string_lossy()
        );
        if event.kind() == kind {
            return event;
        }
    }
    panic!("no {kind:?} event arrived");
}

/// The events a test has taken from a recorder, with the time each arrived.
///
/// A test waits on it for an event about one particular file. Rollover checks
/// assert that the next file opens before the previous one closes, and print
/// the timeline on failure so the report includes native event order and time.
struct Timeline {
    began: Instant,
    events: Vec<(Duration, RecorderEvent)>,
}

impl Timeline {
    fn new() -> Self {
        Self {
            began: Instant::now(),
            events: Vec::new(),
        }
    }

    /// Where the events that follow an operation begin.
    fn mark(&self) -> usize {
        self.events.len()
    }

    /// The path of the first event of `kind` at or after `since`, about
    /// `file` when one is given. Polls `recorder` until such an event
    /// arrives, failing on a recorder error.
    fn await_event(
        &mut self,
        recorder: &mut FileRecorder,
        since: usize,
        kind: RecorderEventKind,
        file: Option<&Path>,
    ) -> PathBuf {
        let deadline = Instant::now() + PATIENCE;
        let mut checked = since;
        loop {
            for (_, event) in &self.events[checked..] {
                let path = PathBuf::from(event.path().to_string_lossy().into_owned());
                if event.kind() == kind && file.is_none_or(|file| file == path) {
                    return path;
                }
            }
            checked = self.events.len();
            assert!(
                Instant::now() < deadline,
                "no {kind:?} event for {file:?} arrived; the recorder reported:\n{self}"
            );
            if let Some(event) = recorder.next_event(Duration::from_millis(50)).unwrap() {
                let failed = matches!(
                    event.kind(),
                    RecorderEventKind::Error | RecorderEventKind::OpenFailed
                );
                self.events.push((self.began.elapsed(), event));
                assert!(!failed, "the recorder failed:\n{self}");
            }
        }
    }

    /// Wait until a change of file requested at `since` has run its course:
    /// the SDK reported the file it opened, the file it `left` closed, and the
    /// first frame written to the new one, which it returns. A test that goes
    /// on only then never overlaps the next change, or its stop, with this one.
    fn await_change(&mut self, recorder: &mut FileRecorder, since: usize, left: &Path) -> PathBuf {
        let next = self.await_event(recorder, since, RecorderEventKind::Rollover, None);
        self.await_event(recorder, since, RecorderEventKind::FileClosed, Some(left));
        self.await_event(recorder, since, RecorderEventKind::FirstFrame, Some(&next));
        let position = |kind, file: &Path| {
            self.events[since..]
                .iter()
                .position(|(_, event)| {
                    event.kind() == kind
                        && Path::new(event.path().to_string_lossy().as_ref()) == file
                })
                .unwrap_or_else(|| panic!("no {kind:?} for {file:?}:\n{self}"))
        };
        assert!(
            position(RecorderEventKind::Rollover, &next)
                < position(RecorderEventKind::FileClosed, left),
            "the next file must open before the previous file closes; recorder finality \
             depends on this native event order:\n{self}"
        );
        assert_eq!(recorder.state(), RecorderState::Recording, "{self}");
        next
    }
}

impl std::fmt::Display for Timeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (at, event) in &self.events {
            writeln!(
                f,
                "  {at:?} {:?} {} {:?}",
                event.kind(),
                event.path().to_string_lossy(),
                event.message().to_string_lossy()
            )?;
        }
        Ok(())
    }
}

/// Fail because `file`, which the SDK reported closed, is not finalized.
///
/// Before failing it keeps probing for as long as a test waits for anything,
/// so that the message tells a finalization that came after the file was
/// reported closed from one that never came.
fn panic_unfinalized(api: &FileApi, file: &Path, info: &FileInfo, timeline: &Timeline) -> ! {
    let reported = Instant::now();
    let bytes = std::fs::metadata(file).map(|metadata| metadata.len());
    let later = loop {
        if crate::file::FilePlayer::probe(api, file).is_ok_and(|info| info.duration().is_final()) {
            break format!("a probe found it finalized {:?} later", reported.elapsed());
        }
        if reported.elapsed() >= PATIENCE {
            break format!("it was still not finalized {PATIENCE:?} later");
        }
        thread::sleep(Duration::from_millis(50));
    };
    panic!(
        "{} was reported closed but is not finalized: duration {} ticks, {bytes:?} bytes; \
         {later}. The recorder reported:\n{timeline}",
        file.display(),
        info.duration().value().ticks()
    );
}

/// Record a short clip of `source` to `file` and return its report.
fn record_clip(api: &FileApi, source: &LiveSource, file: &PathBuf) -> RecordingReport {
    let mut recorder = crate::file::FileRecorder::new(api, &exact_video(source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    thread::sleep(Duration::from_millis(300));
    match recorder.finish(PATIENCE).unwrap() {
        FinishStatus::Ended(SessionEnd::Closed(report)) => report,
        other => panic!("the recording was not confirmed closed: {other:?}"),
    }
}

/// Drive a future to completion on the test thread.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut future = std::pin::pin!(future);
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
        assert!(Instant::now() < deadline, "the operation did not complete");
        thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn the_longest_exact_event_path_records_closes_and_starts_again() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("path-limit");
    let source = LiveSource::start(&ndi, "path-limit");
    let limit = crate::sys::bindings::NDIlib_file_recorder_event_path_max_size as usize - 2;
    let mut directory = scratch.path().to_owned();
    while directory.to_str().unwrap().len() + 1 + 255 < limit {
        directory.push("d".repeat(200));
        std::fs::create_dir(&directory).unwrap();
    }
    let filename_bytes = limit - directory.to_str().unwrap().len() - 1;
    let file = |leading: char, extra: usize| {
        directory.join(format!(
            "{leading}{}.mkv",
            "x".repeat(filename_bytes - 5 + extra)
        ))
    };
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    let unreportable = RecordingTarget::new(file('r', 1)).unwrap();
    assert_eq!(unreportable.path().to_str().unwrap().len(), limit + 1);
    assert!(matches!(
        recorder.start(&unreportable, RecordingTimecode::Preserve),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(recorder.state(), RecorderState::Idle);
    for (session, leading) in [(1, 'a'), (2, 'b')] {
        let target = RecordingTarget::new(file(leading, 0)).unwrap();
        assert_eq!(target.path().to_str().unwrap().len(), limit);
        assert_eq!(
            recorder
                .start(&target, RecordingTimecode::Preserve)
                .unwrap(),
            session
        );
        await_event(&mut recorder, RecorderEventKind::FirstFrame);
        assert_eq!(
            recorder.reported_current_file().unwrap().as_bytes(),
            target.path().to_str().unwrap().as_bytes()
        );
        let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap()
        else {
            panic!("the boundary-length file did not close exactly");
        };
        assert!(report.closure_confirmed());
        assert_eq!(report.closed_files(), 1);
        assert_eq!(report.error_events(), 0);
    }
}

#[test]
fn absolute_recording_names_survive_a_working_directory_change() {
    const CHANGED_DIRECTORY: &str = "GRAFTON_NDI_TEST_RECORDING_CHANGED_DIRECTORY";
    let Some(changed_directory) = std::env::var_os(CHANGED_DIRECTORY) else {
        // The child can change process-wide state without racing other tests.
        let scratch = FixtureDir::create("recording-cwd");
        let original = scratch.join("original");
        let changed = scratch.join("changed");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&changed).unwrap();
        let output = crate::test_support::TestChild::new(
            "file::sdk_tests::absolute_recording_names_survive_a_working_directory_change",
        )
        .current_dir(original)
        .env(CHANGED_DIRECTORY, changed)
        .run();
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    };
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let source = LiveSource::start(&ndi, "recording-cwd");
    let original = std::env::current_dir().unwrap();
    let target = RecordingTarget::new("take").unwrap();
    let recorder = |pattern| {
        let options = FileRecorderOptions::new(source.spec.clone())
            .naming(FileNaming::Pattern(pattern))
            .proxy(false)
            .tracks(MediaTypes::VIDEO);
        FileRecorder::new(&api, &options).unwrap()
    };
    let refuse_start = |pattern| {
        let mut recorder = recorder(pattern);
        let result = recorder.start(&target, RecordingTimecode::Preserve);
        assert_eq!(recorder.state(), RecorderState::Idle);
        assert!(!recorder.is_recording());
        assert_eq!(recorder.report().session(), 0);
        result.unwrap_err()
    };
    assert!(matches!(
        refuse_start("take{index}.mkv".to_owned()),
        Error::InvalidInput(crate::InputDefect::RecordingPathNotAbsolute)
    ));

    // Normalizing repeated `./` used to make the checked path short enough,
    // although the SDK emitted a path at the event field's truncation boundary.
    let event_limit = crate::sys::bindings::NDIlib_file_recorder_event_path_max_size as usize - 2;
    let long_relative = format!(
        "{}x.mkv",
        "./".repeat((event_limit + 1 - "x.mkv".len()) / 2)
    );
    assert_eq!(long_relative.len(), event_limit + 1);
    assert!(matches!(
        refuse_start(long_relative),
        Error::InvalidInput(crate::InputDefect::RecordingPathTooLong {
            maximum_bytes,
            longest_bytes,
            index_digits_reserved: 0,
        }) if maximum_bytes == event_limit && longest_bytes == event_limit + 1
    ));
    assert!(std::fs::read_dir(&original).unwrap().next().is_none());

    let mut recorder = recorder("{prefix}{index}.mkv".to_owned());
    recorder
        .start(&target, RecordingTimecode::Preserve)
        .unwrap();
    let first = original.join("take1.mkv");
    let second = original.join("take2.mkv");
    let assert_event_path = |event_path: &EventText, expected: &Path| {
        assert_eq!(event_path.as_bytes(), expected.to_str().unwrap().as_bytes());
        assert!(!event_path.is_possibly_truncated());
    };
    let started = await_event(&mut recorder, RecorderEventKind::Started);
    assert_event_path(started.path(), &first);
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    assert_event_path(recorder.reported_current_file().unwrap(), &first);

    std::env::set_current_dir(&changed_directory).unwrap();
    source.change_format();
    let mut timeline = Timeline::new();
    assert_eq!(timeline.await_change(&mut recorder, 0, &first), second);
    assert_event_path(recorder.reported_current_file().unwrap(), &second);

    let outcome = recorder.finish(PATIENCE).unwrap();
    let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
        panic!("the directory change made closure indeterminate: {outcome:?}\n{timeline}");
    };
    assert_eq!(report.closed_files(), 2);
    assert_eq!(report.error_events(), 0);
    assert_event_path(report.last_closed_path().unwrap(), &second);
    assert!(first.is_file());
    assert!(second.is_file());
    assert!(std::fs::read_dir(changed_directory)
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn a_recording_is_finalized_and_probes_with_its_video_track() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("record");
    let source = LiveSource::start(&ndi, "record");
    let file = scratch.join("take.mkv");

    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    assert_eq!(recorder.state(), RecorderState::Idle);
    assert_eq!(recorder.last_accepted_source(), Some(&source.spec));
    let session = recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    assert_eq!(session, 1);
    let first = await_event(&mut recorder, RecorderEventKind::FirstFrame);
    assert_eq!(first.source_format().unwrap().video_tracks().len(), 1);
    assert_live_video(&first.source_format().unwrap().video_tracks()[0]);
    assert!(first.tracks().is_empty());
    assert_eq!(
        recorder.reported_current_file().unwrap().as_bytes(),
        file.to_str().unwrap().as_bytes()
    );
    assert!(recorder.is_recording());
    assert!(recorder.source_format().unwrap().is_some());
    thread::sleep(Duration::from_millis(300));
    let stats = recorder.stats().unwrap().expect("counters while recording");
    assert!(stats.video_frames_written() > 0);

    let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap() else {
        panic!("the recording was not confirmed closed");
    };
    assert!(report.closure_confirmed());
    assert!(!report.stopped_by_sdk());
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.empty_files(), 0);
    assert_eq!(report.error_events(), 0);
    assert_eq!(recorder.state(), RecorderState::Closed);
    assert!(!recorder.is_recording());

    let info = crate::file::FilePlayer::probe(&api, &file).unwrap();
    assert_eq!(info.video_track_count(), 1);
    assert_live_video(&info.source_format().unwrap().video_tracks()[0]);
    assert!(info.duration().is_final());
    assert!(info.duration().value().ticks() > 0);
    assert!(matches!(
        crate::file::FilePlayer::probe(&api, scratch.join("absent.mkv")),
        Err(Error::NativeOperationFailed { .. })
    ));
}

#[test]
fn a_rollover_requested_just_before_the_stop_still_ends_in_a_confirmed_close() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("late-rollover");
    let source = LiveSource::start(&ndi, "late-rollover");
    let options = FileRecorderOptions::new(source.spec.clone())
        .proxy(false)
        .tracks(MediaTypes::VIDEO);
    let mut recorder = crate::file::FileRecorder::new(&api, &options).unwrap();
    recorder
        .start(
            &RecordingTarget::new(scratch.join("first")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    recorder
        .rollover(&RecordingTarget::new(scratch.join("second")).unwrap())
        .unwrap();
    let outcome = recorder.finish(PATIENCE).unwrap();
    let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
        panic!("the session did not reach a confirmed close: {outcome:?}");
    };
    assert!(report.closed_files() >= 1);
}

#[test]
fn rollovers_and_a_source_switch_end_in_a_confirmed_close_of_every_file() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("rollovers");
    let source = LiveSource::start(&ndi, "rollovers");
    let replacement = LiveSource::start(&ndi, "rollovers-replacement");
    assert_ne!(source.spec, replacement.spec);
    let options = FileRecorderOptions::new(source.spec.clone())
        .proxy(false)
        .tracks(MediaTypes::VIDEO);
    let mut recorder = crate::file::FileRecorder::new(&api, &options).unwrap();
    let mut timeline = Timeline::new();
    let started = timeline.mark();
    recorder
        .start(
            &RecordingTarget::new(scratch.join("first")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    let first = timeline.await_event(&mut recorder, started, RecorderEventKind::Started, None);
    timeline.await_event(
        &mut recorder,
        started,
        RecorderEventKind::FirstFrame,
        Some(&first),
    );

    let since = timeline.mark();
    recorder
        .rollover(&RecordingTarget::new(scratch.join("second")).unwrap())
        .unwrap();
    let second = timeline.await_change(&mut recorder, since, &first);
    let since = timeline.mark();
    recorder.set_source(&replacement.spec).unwrap();
    let third = timeline.await_change(&mut recorder, since, &second);
    let switched = second.clone();
    let files = [first, second, third];

    let outcome = recorder.finish(PATIENCE).unwrap();
    let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
        panic!("the session did not reach a confirmed close: {outcome:?}\n{timeline}");
    };
    assert_eq!(report.closed_files(), 3, "{timeline}");
    let mut written: Vec<_> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    written.sort();
    let mut reported = files.to_vec();
    reported.sort();
    assert_eq!(written, reported, "the files on disk:\n{timeline}");
    for file in &files {
        let info = crate::file::FilePlayer::probe(&api, file).unwrap();
        if info.duration().is_final() {
            continue;
        }
        // The macOS File library can leave the file that a source switch
        // closes unfinalized, as measured at:
        // https://github.com/GrantSparks/grafton-ndi/blob/226fa3ef36196bd20c3f0060c3fa0e233b700ace/README.md#file-library-ndi-64
        if cfg!(target_os = "macos") && *file == switched {
            test_support::unsupported(
                "finalizing a file that a source switch closes, which the macOS File library can \
                 leave undone",
            );
            continue;
        }
        panic_unfinalized(&api, file, &info, &timeline);
    }
}

#[test]
fn rollovers_open_the_next_file_before_closing_the_previous_file() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("rollover-order");
    let source = LiveSource::start(&ndi, "rollover-order");
    // Sixteen changes per case pin thirty-two rollovers for each mechanism,
    // including recording both with and without the source's proxy stream.
    const CHANGES: usize = 16;
    for proxy in [false, true] {
        for mechanism in ["explicit", "source", "size"] {
            let prefix = format!("{mechanism}-proxy-{proxy}");
            let options = FileRecorderOptions::new(source.spec.clone())
                .naming(FileNaming::Indexed)
                .proxy(proxy)
                .tracks(MediaTypes::VIDEO)
                .rollover_size(if mechanism == "size" { 65_536 } else { 0 });
            let mut recorder = crate::file::FileRecorder::new(&api, &options).unwrap();
            let mut timeline = Timeline::new();
            recorder
                .start(
                    &RecordingTarget::new(scratch.join(&prefix)).unwrap(),
                    RecordingTimecode::Preserve,
                )
                .unwrap();
            let mut current =
                timeline.await_event(&mut recorder, 0, RecorderEventKind::Started, None);
            timeline.await_event(
                &mut recorder,
                0,
                RecorderEventKind::FirstFrame,
                Some(&current),
            );
            for index in 0..CHANGES {
                let since = timeline.mark();
                match mechanism {
                    "explicit" => recorder
                        .rollover(
                            &RecordingTarget::new(scratch.join(format!("{prefix}-{index}")))
                                .unwrap(),
                        )
                        .unwrap(),
                    "source" => recorder.set_source(&source.spec).unwrap(),
                    "size" => {} // The configured size threshold triggers this change.
                    _ => unreachable!(),
                }
                current = timeline.await_change(&mut recorder, since, &current);
            }
            let outcome = recorder.finish(PATIENCE).unwrap();
            let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
                panic!("{prefix} did not reach confirmed closure: {outcome:?}\n{timeline}");
            };
            assert!(report.closure_confirmed(), "{prefix}: {timeline}");
            assert!(
                report.closed_files() > CHANGES as u64,
                "{prefix}: {timeline}"
            );
            assert_eq!(report.dropped_events(), 0, "{prefix}: {timeline}");
        }
    }
}

#[test]
fn generated_recording_timecode_replays_its_explicit_start() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("generated-timecode");
    let source = LiveSource::start(&ndi, "generated-timecode");
    let file = scratch.join("timecode.mkv");
    let start = crate::Timecode::from_ticks(360_000_000_000);
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::GenerateFrom(SessionTimecode::new(start).unwrap()),
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    thread::sleep(Duration::from_millis(500));
    assert!(matches!(
        recorder.finish(PATIENCE).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
    assert!(crate::file::FilePlayer::probe(&api, &file)
        .unwrap()
        .duration()
        .is_final());

    let name = format!("grafton-generated-timecode-player-{}", std::process::id());
    let mut player = crate::file::FilePlayer::new(&api, &FilePlayerOptions::new(name)).unwrap();
    player.start(&file).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no first playback frame event arrived"
        );
        let Some(event) = player.next_event(Duration::from_millis(50)).unwrap() else {
            continue;
        };
        assert_ne!(event.kind(), PlayerEventKind::Error, "{event:?}");
        if event.kind() == PlayerEventKind::FirstFrame {
            assert_eq!(event.source_timecode(), Some(start));
            break;
        }
    }
    let mut previous = start;
    let mut advancing_samples = 0;
    while advancing_samples < 3 {
        assert!(
            Instant::now() < deadline,
            "playback timecodes did not advance from {start:?}"
        );
        let event = player.next_event(Duration::from_millis(50)).unwrap();
        assert!(event.is_none_or(|event| event.kind() != PlayerEventKind::Error));
        let timecode = player
            .source_timecode()
            .expect("the last displayed frame has a timecode");
        assert!(timecode >= previous, "{timecode:?} preceded {previous:?}");
        assert_ne!(timecode, crate::Timecode::SYNTHESIZE);
        if timecode > previous {
            advancing_samples += 1;
        }
        previous = timecode;
    }
}

#[test]
fn injected_metadata_is_recorded_on_the_metadata_track() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("metadata");
    let source = LiveSource::start(&ndi, "metadata");
    let file = scratch.join("marked.mkv");
    let options = FileRecorderOptions::new(source.spec.clone())
        .naming(FileNaming::Exact)
        .proxy(false)
        .tracks(MediaTypes::VIDEO | MediaTypes::METADATA);
    let mut recorder = crate::file::FileRecorder::new(&api, &options).unwrap();
    let marker = crate::MetadataFrame::new("<marker cue=\"intro\"/>")
        .unwrap()
        .with_timecode(crate::Timecode::from_ticks(0));
    assert!(matches!(
        recorder.inject_metadata(&marker),
        Err(Error::InvalidState(StateConflict::NotRecording))
    ));
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::GenerateFrom(
                SessionTimecode::new(crate::Timecode::from_ticks(36_000_000_000)).unwrap(),
            ),
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    recorder.inject_metadata(&marker).unwrap();
    thread::sleep(Duration::from_millis(200));
    assert!(matches!(
        recorder.finish(PATIENCE).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
    assert_eq!(
        crate::file::FilePlayer::probe(&api, &file)
            .unwrap()
            .metadata_track_count(),
        1
    );
}

/// What a drop leaves behind with the real library.
///
/// This cannot show that the drop waited for the file to be closed: the 6.4
/// library finalizes the file inside the stop call, so the file is final even
/// when the recorder is destroyed straight after it, as `abandon` does. The
/// wait is tested against the stand-in library, whose close can come late, in
/// `dropping_a_recording_recorder_waits_for_its_file_to_be_closed`.
#[test]
fn dropping_a_recorder_leaves_a_finalized_file() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("drop");
    let source = LiveSource::start(&ndi, "drop");
    let file = scratch.join("dropped.mkv");
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    thread::sleep(Duration::from_millis(300));
    drop(recorder);
    let info = crate::file::FilePlayer::probe(&api, &file).unwrap();
    assert!(info.duration().is_final());
    assert!(info.duration().value().ticks() > 0);
}

#[test]
fn dropping_an_async_recorder_leaves_a_finalized_file() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("async-drop");
    let source = LiveSource::start(&ndi, "async-drop");
    let file = scratch.join("dropped.mkv");
    let recorder = block_on(AsyncFileRecorder::create(&api, &exact_video(&source))).unwrap();
    block_on(recorder.start(
        &RecordingTarget::new(&file).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let event = block_on(recorder.next_event(Duration::from_millis(100))).unwrap();
        if event.is_some_and(|event| event.kind() == RecorderEventKind::FirstFrame) {
            break;
        }
        assert!(Instant::now() < deadline, "no first frame was recorded");
    }
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        block_on(recorder.state()).unwrap(),
        RecorderState::Recording
    );
    drop(recorder);
    // No wait here: the drop itself returns only once the file is finalized.
    let info = crate::file::FilePlayer::probe(&api, &file).unwrap();
    assert!(info.duration().is_final());
    assert!(info.duration().value().ticks() > 0);
}

#[test]
fn an_async_recorder_finishes_and_closes_without_blocking() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("async-finish");
    let source = LiveSource::start(&ndi, "async-finish");
    let file = scratch.join("finished.mkv");
    let recorder = block_on(AsyncFileRecorder::create(&api, &exact_video(&source))).unwrap();
    block_on(recorder.start(
        &RecordingTarget::new(&file).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(800));
    let outcome = block_on(recorder.finish(PATIENCE)).unwrap();
    let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
        panic!("the recording was not confirmed closed: {outcome:?}");
    };
    assert_eq!(report.closed_files(), 1);
    let previous = block_on(recorder.reset(&exact_video(&source))).unwrap();
    assert_eq!(previous.session(), report.session());
    assert!(previous.closure_confirmed());
    assert_eq!(block_on(recorder.state()).unwrap(), RecorderState::Idle);
    let next_file = scratch.join("after-reset.mkv");
    let next_session = block_on(recorder.start(
        &RecordingTarget::new(&next_file).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    assert_eq!(next_session, report.session() + 1);
    let deadline = Instant::now() + PATIENCE;
    while block_on(recorder.stats())
        .unwrap()
        .is_none_or(|stats| stats.video_frames_written() == 0)
    {
        assert!(
            Instant::now() < deadline,
            "nothing was recorded after reset"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        block_on(recorder.finish(PATIENCE)).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
    assert!(crate::file::FilePlayer::probe(&api, &next_file)
        .unwrap()
        .duration()
        .is_final());
    let retained = recorder.clone();
    block_on(recorder.close()).unwrap();
    assert!(
        block_on(retained.state()).is_err(),
        "a close ends every clone of the handle"
    );
    assert!(crate::file::FilePlayer::probe(&api, &file)
        .unwrap()
        .duration()
        .is_final());
}

#[test]
fn a_player_plays_a_recording_and_keeps_its_source_between_files() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("player");
    let clip = scratch.join("clip.mkv");
    record_clip(&api, &LiveSource::start(&ndi, "player"), &clip);

    let name = format!("grafton-file-player-{}", std::process::id());
    let mut player = crate::file::FilePlayer::new(&api, &FilePlayerOptions::new(&name)).unwrap();
    let source = player.advertised_source().unwrap();
    assert!(source.name().contains(&name), "{source:?}");
    assert!(!player.is_running());
    player.set_paused(true);
    assert!(
        !player.is_paused(),
        "pausing has no effect with no file open"
    );
    player.set_looping(true);
    assert!(matches!(
        player.start(scratch.join("absent.mkv")),
        Err(Error::NativeOperationFailed { .. })
    ));

    player.start(&clip).unwrap();
    assert!(player.is_running());
    assert!(player.is_looping());
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "the file was not reported open");
        let Some(event) = player.next_event(Duration::from_millis(50)).unwrap() else {
            continue;
        };
        assert_ne!(
            event.kind(),
            PlayerEventKind::Error,
            "{}",
            event.message().to_string_lossy()
        );
        if event.kind() == PlayerEventKind::FileOpened {
            break;
        }
    }
    player.set_paused(true);
    assert!(player.is_paused());
    assert_eq!(
        player
            .source_format()
            .unwrap()
            .unwrap()
            .video_tracks()
            .len(),
        1
    );
    assert!(player
        .duration()
        .is_some_and(|duration| duration.is_final()));
    player.seek(FilePosition::from_ticks(0)).unwrap();
    player.set_paused(false);
    assert!(player.stats().unwrap().is_some());

    player.stop();
    assert!(!player.is_running());
    assert!(!player.is_paused());
    assert!(player.is_looping(), "looping belongs to the player");
    assert_eq!(
        player.advertised_source().unwrap(),
        source,
        "a stop keeps the advertised source"
    );
    // A second file plays through the same source.
    player.start(&clip).unwrap();
    assert!(player.is_running());
}

#[test]
fn an_async_player_runs_on_its_own_thread_and_closes_every_clone() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("async-player");
    let clip = scratch.join("clip.mkv");
    record_clip(&api, &LiveSource::start(&ndi, "async-player"), &clip);

    assert_eq!(
        block_on(AsyncFilePlayer::probe(&api, &clip))
            .unwrap()
            .video_track_count(),
        1
    );
    let name = format!("grafton-file-async-player-{}", std::process::id());
    let player = block_on(AsyncFilePlayer::create(
        &api,
        &FilePlayerOptions::new(&name),
    ))
    .unwrap();
    let debug = format!("{player:?}");
    assert!(debug.contains(&name));
    assert!(debug.ends_with(", .. }"));
    assert_eq!(format!("{:?}", player.clone()), debug);
    assert!(block_on(player.advertised_source())
        .unwrap()
        .name()
        .contains(&name));
    block_on(player.set_looping(true)).unwrap();
    block_on(player.start(&clip)).unwrap();
    assert!(block_on(player.is_running()).unwrap());
    assert!(block_on(player.is_looping()).unwrap());
    block_on(player.set_paused(true)).unwrap();
    assert!(block_on(player.is_paused()).unwrap());
    assert!(block_on(player.source_format()).unwrap().is_some());
    let retained = player.clone();
    block_on(player.close()).unwrap();
    assert!(
        block_on(retained.is_running()).is_err(),
        "a close ends every clone of the handle"
    );
}

#[test]
fn the_file_library_stays_selected_after_its_handles_are_dropped() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let selected = api.library_path().map(PathBuf::from);
    drop(api);
    let reopened = FileApi::new(&ndi, &FileApiOptions::default()).unwrap();
    assert_eq!(reopened.library_path().map(PathBuf::from), selected);
    // Its entry points are still callable.
    assert!(matches!(
        crate::file::FilePlayer::probe(
            &reopened,
            std::env::temp_dir().join("grafton-ndi-absent.mkv")
        ),
        Err(Error::NativeOperationFailed { .. })
    ));
    if let Some(selected) = selected {
        assert!(matches!(
            FileApi::new(
                &ndi,
                &FileApiOptions::default().library_path(selected.with_file_name("other"))
            ),
            Err(Error::RuntimeConflict(failure)) if failure.kind() == crate::SelectionConflict::DifferentLibrary
        ));
        FileApi::new(&ndi, &FileApiOptions::default().library_path(selected)).unwrap();
    }
}

/// Options under which a format change ends the session instead of rolling over.
fn stop_on_format_change(source: &LiveSource) -> FileRecorderOptions {
    FileRecorderOptions::new(source.spec.clone())
        .proxy(false)
        .tracks(MediaTypes::VIDEO)
        .format_change_rollover(false)
}

#[test]
fn a_session_the_library_ended_on_a_format_change_can_be_followed_by_another() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("format-stop");
    let source = LiveSource::start(&ndi, "format-stop");
    let mut recorder =
        crate::file::FileRecorder::new(&api, &stop_on_format_change(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(scratch.join("first")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);

    source.change_format();
    // The library closes the file and ends the session without being asked.
    let deadline = Instant::now() + PATIENCE;
    while recorder.state() != RecorderState::Closed {
        assert!(Instant::now() < deadline, "{:?}", recorder.state());
        recorder.next_event(Duration::from_millis(50)).unwrap();
    }
    let report = recorder.report();
    assert!(report.closure_confirmed());
    assert!(report.stopped_by_sdk());
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.error_events(), 1);
    assert_eq!(recorder.request_stop(), StopRequest::AlreadyClosed);

    let session = recorder
        .start(
            &RecordingTarget::new(scratch.join("second")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    assert_eq!(session, 2);
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    assert!(matches!(
        recorder.finish(PATIENCE).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
}

#[test]
fn an_async_session_the_library_ended_on_a_format_change_can_be_followed_by_another() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("async-format-stop");
    let source = LiveSource::start(&ndi, "async-format-stop");
    let recorder = block_on(AsyncFileRecorder::create(
        &api,
        &stop_on_format_change(&source),
    ))
    .unwrap();
    block_on(recorder.start(
        &RecordingTarget::new(scratch.join("first")).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    let state_becomes = |wanted: RecorderState| {
        let deadline = Instant::now() + PATIENCE;
        while block_on(recorder.state()).unwrap() != wanted {
            assert!(
                Instant::now() < deadline,
                "the recorder never became {wanted:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    // Frames are being written once the library reports counters for them.
    let deadline = Instant::now() + PATIENCE;
    while block_on(recorder.stats())
        .unwrap()
        .is_none_or(|stats| stats.video_frames_written() == 0)
    {
        assert!(Instant::now() < deadline, "nothing was recorded");
        thread::sleep(Duration::from_millis(20));
    }

    source.change_format();
    state_becomes(RecorderState::Closed);
    let session = block_on(recorder.start(
        &RecordingTarget::new(scratch.join("second")).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    assert_eq!(session, 2);
    state_becomes(RecorderState::Recording);
    assert!(matches!(
        block_on(recorder.finish(PATIENCE)).unwrap(),
        FinishStatus::Ended(SessionEnd::Closed(_))
    ));
    block_on(recorder.close()).unwrap();
}

#[test]
fn a_rollover_into_a_missing_directory_is_refused_and_the_session_goes_on() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("missing-directory");
    let source = LiveSource::start(&ndi, "missing-directory");
    let missing = || RecordingTarget::new(scratch.join("absent/take")).unwrap();
    let not_found = |result: Result<()>| match result {
        Err(Error::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected the directory to be reported missing: {other:?}"),
    };
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    not_found(
        recorder
            .start(&missing(), RecordingTimecode::Preserve)
            .map(drop),
    );
    recorder
        .start(
            &RecordingTarget::new(scratch.join("take.mkv")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);

    // Passed on, the library would accept this and then end the session.
    not_found(recorder.rollover(&missing()));
    assert_eq!(recorder.state(), RecorderState::Recording);
    assert!(recorder.is_recording());
    thread::sleep(Duration::from_millis(200));
    let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap() else {
        panic!("the recording was not confirmed closed");
    };
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.error_events(), 0);
}

#[test]
#[cfg(target_os = "linux")]
fn an_unrepresentable_rollover_candidate_leaves_the_session_recording() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("long-rollover-name");
    let source = LiveSource::start(&ndi, "long-rollover-name");
    let invalid = RecordingTarget::new(scratch.join("x".repeat(256))).unwrap();
    let live = RecordingTarget::new(scratch.join("take.mkv")).unwrap();
    for naming in [FileNaming::Exact, FileNaming::Indexed] {
        let options = exact_video(&source).naming(naming);
        let mut recorder = FileRecorder::new(&api, &options).unwrap();
        recorder.start(&live, RecordingTimecode::Preserve).unwrap();
        await_event(&mut recorder, RecorderEventKind::FirstFrame);

        let Err(Error::Io(error)) = recorder.rollover(&invalid) else {
            panic!("the filesystem should reject the candidate at preflight");
        };
        assert_eq!(error.raw_os_error(), Some(libc::ENAMETOOLONG));
        assert_eq!(recorder.state(), RecorderState::Recording);
        assert!(recorder.is_recording());
        let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap()
        else {
            panic!("the recording was not confirmed closed");
        };
        assert!(report.closure_confirmed());
        assert!(!report.stopped_by_sdk());
        assert_eq!(report.closed_files(), 1);
        assert_eq!(report.error_events(), 0);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn an_indexed_rollover_uses_the_next_free_candidate() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("indexed-rollover");
    let source = LiveSource::start(&ndi, "indexed-rollover");
    let occupied = scratch.join("second_001.mkv");
    std::fs::write(&occupied, b"kept").unwrap();
    let options = exact_video(&source).naming(FileNaming::Indexed);
    let mut recorder = FileRecorder::new(&api, &options).unwrap();
    recorder
        .start(
            &RecordingTarget::new(scratch.join("first")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    let mut timeline = Timeline::new();
    let first = timeline.await_event(&mut recorder, 0, RecorderEventKind::FirstFrame, None);
    let since = timeline.mark();
    recorder
        .rollover(&RecordingTarget::new(scratch.join("second")).unwrap())
        .unwrap();
    assert_eq!(
        timeline.await_change(&mut recorder, since, &first),
        scratch.join("second_002.mkv")
    );
    let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap() else {
        panic!("the recording was not confirmed closed");
    };
    assert_eq!(report.closed_files(), 2);
    assert_eq!(report.error_events(), 0);
    assert_eq!(std::fs::read(occupied).unwrap(), b"kept");
}

#[test]
fn a_file_that_received_nothing_is_reported_as_empty() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("empty-file");
    let source = LiveSource::start(&ndi, "empty-file");
    let options = FileRecorderOptions::new(source.spec.clone())
        .proxy(false)
        .tracks(MediaTypes::VIDEO);
    let mut recorder = crate::file::FileRecorder::new(&api, &options).unwrap();
    recorder
        .start(
            &RecordingTarget::new(scratch.join("take")).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    // Nothing answers at this address, so the file rolled over to stays empty.
    recorder
        .set_source(&SourceSpec::from_url("127.0.0.1:9").unwrap())
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::Rollover);

    // The library reports both files closed all the same.
    let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap() else {
        panic!("the recording was not confirmed closed");
    };
    assert_eq!(report.closed_files(), 2);
    assert_eq!(report.empty_files(), 1);
    let last = PathBuf::from(report.last_closed_path().unwrap().to_str().unwrap());
    assert_eq!(std::fs::metadata(last).unwrap().len(), 0);
}

/// A NUL-terminated copy of `text` that a test can overwrite.
fn c_buffer(text: &str) -> Vec<u8> {
    std::ffi::CString::new(text).unwrap().into_bytes_with_nul()
}

/// Overwrite a string in place, keeping its length and terminator, the way
/// freed and reused memory would look to anything still pointing at it.
fn overwrite(buffer: &mut [u8]) {
    let end = buffer.len() - 1;
    buffer[..end].fill(b'Z');
}

#[test]
fn the_library_copies_every_string_a_recorder_gives_it() {
    use crate::sys::bindings::*;
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("recorder-strings");
    let source = LiveSource::start(&ndi, "recorder-strings");
    let recording = api.table::<crate::sys::FileRecording>().unwrap();
    let address = source.spec.url().unwrap().to_owned();
    let file = |name: &str| scratch.join(name).to_str().unwrap().to_owned();

    let mut url = c_buffer(&address);
    let mut pattern = c_buffer("{prefix}_{index:03}.mkv");
    let settings = NDIlib_file_recorder_create_t {
        source_to_connect_to: NDIlib_file_recorder_source_t {
            p_ndi_name: std::ptr::null(),
            p_url_address: url.as_ptr().cast(),
        },
        track_flags: NDIlib_file_recorder_track_flags_e_NDIlib_file_recorder_track_flags_video,
        record_float_audio_planar_32f: false,
        disable_auto_rollover_on_format_change: false,
        p_output_filename_pattern: pattern.as_ptr().cast(),
        disable_proxy_track: true,
        rollover_max_file_size_bytes: 0,
    };
    // SAFETY: the settings and their strings are valid for the call.
    let handle = unsafe { recording.NDIlib_file_recorder_create(&settings) };
    assert!(!handle.is_null());
    overwrite(&mut url);
    overwrite(&mut pattern);

    // The next event of `kind`, with the path it names.
    let next = |kind: NDIlib_file_recorder_event_type_e| {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "no event of type {kind} arrived");
            let mut event = OutParam::<NDIlib_file_recorder_event_t>::new();
            // SAFETY: the recorder is live and `event` is storage for one event.
            let got = unsafe {
                recording.NDIlib_file_recorder_capture_event(handle, event.as_mut_ptr(), 50)
            };
            if got == kind {
                let event = event.read().unwrap();
                return EventText::from_buffer(&event.p_path)
                    .to_string_lossy()
                    .into_owned();
            }
        }
    };
    let started = NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_started;
    let rolled = NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_rollover;
    let first_frame = NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_first_frame;
    let closed = NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_file_closed;

    let mut prefix = c_buffer(&file("first"));
    // SAFETY: the recorder is live and `prefix` is valid for the call.
    assert!(unsafe { recording.NDIlib_file_recorder_start(handle, prefix.as_ptr().cast()) });
    overwrite(&mut prefix);
    // The pattern and the prefix made the name, and the address reached the
    // source, although all three were overwritten as soon as their calls returned.
    assert_eq!(next(started), file("first_001.mkv"));
    assert_eq!(next(first_frame), file("first_001.mkv"));

    // A rollover uses its prefix after the call has returned, at the cut.
    let mut prefix = c_buffer(&file("second"));
    // SAFETY: the recorder is live and `prefix` is valid for the call.
    assert!(unsafe { recording.NDIlib_file_recorder_rollover(handle, prefix.as_ptr().cast()) });
    overwrite(&mut prefix);
    assert_eq!(next(rolled), file("second_001.mkv"));
    assert_eq!(next(first_frame), file("second_001.mkv"));

    let mut url = c_buffer(&address);
    let descriptor = NDIlib_file_recorder_source_t {
        p_ndi_name: std::ptr::null(),
        p_url_address: url.as_ptr().cast(),
    };
    // SAFETY: the recorder is live; the descriptor and its string are valid
    // for the call.
    assert!(unsafe { recording.NDIlib_file_recorder_set_source(handle, &descriptor) });
    overwrite(&mut url);
    assert_eq!(next(rolled), file("second_002.mkv"));
    assert_eq!(next(first_frame), file("second_002.mkv"));

    // SAFETY: the recorder is live, and is destroyed once after its last use.
    unsafe {
        recording.NDIlib_file_recorder_stop(handle);
        assert_eq!(next(closed), file("second_002.mkv"));
        recording.NDIlib_file_recorder_destroy(handle);
    }
}

#[test]
fn the_library_copies_every_string_a_player_gives_it() {
    use crate::sys::bindings::*;
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("player-strings");
    let clip = scratch.join("clip.mkv");
    record_clip(&api, &LiveSource::start(&ndi, "player-strings"), &clip);
    let playback = api.table::<crate::sys::FilePlayback>().unwrap();

    let advertised = format!("grafton-file-strings-{}", std::process::id());
    let mut name = c_buffer(&advertised);
    let settings = NDIlib_file_player_create_t {
        p_ndi_send_name: name.as_ptr().cast(),
        p_groups: std::ptr::null(),
    };
    // SAFETY: the settings and their string are valid for the call.
    let handle = unsafe { playback.NDIlib_file_player_create(&settings) };
    assert!(!handle.is_null());
    overwrite(&mut name);
    // SAFETY: the player is live, and the descriptor it returns stays valid
    // until the player is destroyed.
    let source = unsafe {
        crate::Source::try_from_raw(
            playback
                .named()
                .NDIlib_file_player_get_advertised_source(handle),
        )
    }
    .unwrap();
    assert!(source.name().contains(&advertised), "{source:?}");

    let mut path = c_buffer(clip.to_str().unwrap());
    // SAFETY: the player is live and `path` is valid for the call.
    assert!(unsafe { playback.NDIlib_file_player_start(handle, path.as_ptr().cast()) });
    overwrite(&mut path);
    let opened = NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_opened;
    let deadline = Instant::now() + PATIENCE;
    let reported = loop {
        assert!(Instant::now() < deadline, "the file was not reported open");
        let mut event = OutParam::<NDIlib_file_player_event_t>::new();
        // SAFETY: the player is live and `event` is storage for one event.
        let got =
            unsafe { playback.NDIlib_file_player_capture_event(handle, event.as_mut_ptr(), 50) };
        if got == opened {
            let event = event.read().unwrap();
            break EventText::from_buffer(&event.p_path)
                .to_string_lossy()
                .into_owned();
        }
    };
    assert_eq!(reported, clip.to_str().unwrap());
    // SAFETY: the player is live, and is destroyed once after its last use.
    unsafe {
        playback.NDIlib_file_player_stop(handle);
        playback.NDIlib_file_player_destroy(handle);
    }
}

/// Poll until the recorder's state is `wanted`.
fn await_state(recorder: &mut FileRecorder, wanted: RecorderState) {
    let deadline = Instant::now() + PATIENCE;
    while recorder.state() != wanted {
        assert!(Instant::now() < deadline, "{:?}", recorder.state());
        recorder.next_event(Duration::from_millis(50)).unwrap();
    }
}

#[test]
fn an_exact_recording_ends_at_a_format_change_with_its_file_finalized() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("exact-format");
    let source = LiveSource::start(&ndi, "exact-format");
    let file = scratch.join("take.mkv");
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);

    source.change_format();
    // With no second name to roll over to, the library closes the file and
    // ends the session instead of failing to open a file of the same name.
    await_state(&mut recorder, RecorderState::Closed);
    let report = recorder.report();
    assert!(report.closure_confirmed());
    assert!(report.stopped_by_sdk());
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.empty_files(), 0);
    let FinishStatus::Ended(SessionEnd::Closed(finished)) = recorder.finish(PATIENCE).unwrap()
    else {
        panic!("the session had been closed");
    };
    assert!(finished.stopped_by_sdk());
    let info = crate::file::FilePlayer::probe(&api, &file).unwrap();
    assert!(info.duration().is_final());
    assert!(info.duration().value().ticks() > 0);
}

#[test]
fn an_exact_session_refuses_a_change_of_source_and_keeps_recording() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("exact-source");
    let source = LiveSource::start(&ndi, "exact-source");
    let file = scratch.join("take.mkv");
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);

    assert!(matches!(
        recorder.set_source(&source.spec),
        Err(Error::InvalidState(StateConflict::ExactFileName))
    ));
    let before = recorder.stats().unwrap().unwrap().video_frames_written();
    let deadline = Instant::now() + PATIENCE;
    while recorder.stats().unwrap().unwrap().video_frames_written() <= before {
        assert!(Instant::now() < deadline, "the session stopped recording");
        assert!(recorder
            .next_event(Duration::from_millis(20))
            .unwrap()
            .is_none_or(|event| {
                !matches!(
                    event.kind(),
                    RecorderEventKind::Error | RecorderEventKind::OpenFailed
                )
            }));
    }
    assert!(recorder.is_recording());
    assert_eq!(recorder.state(), RecorderState::Recording);

    let FinishStatus::Ended(SessionEnd::Closed(report)) = recorder.finish(PATIENCE).unwrap() else {
        panic!("the recording was not confirmed closed");
    };
    assert!(!report.stopped_by_sdk());
    assert_eq!(report.closed_files(), 1);
    assert_eq!(report.error_events(), 0);
    assert!(crate::file::FilePlayer::probe(&api, &file)
        .unwrap()
        .duration()
        .is_final());
}

#[test]
fn an_exact_target_that_exists_is_refused_before_the_library_sees_it() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("exact-exists");
    let source = LiveSource::start(&ndi, "exact-exists");
    let file = scratch.join("taken.mkv");
    std::fs::write(&file, b"kept").unwrap();
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    match recorder.start(
        &RecordingTarget::new(&file).unwrap(),
        RecordingTimecode::Preserve,
    ) {
        Err(Error::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        }
        other => panic!("expected the existing file to be refused: {other:?}"),
    }
    // Had the library been asked, it would have queued an open-failed event.
    assert!(recorder
        .next_event(Duration::from_millis(200))
        .unwrap()
        .is_none());
    assert_eq!(recorder.state(), RecorderState::Idle);
    assert!(!recorder.is_recording());
    assert_eq!(std::fs::read(&file).unwrap(), b"kept");
}

#[test]
fn finishing_without_a_wait_reports_the_close_the_stop_queued() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = FixtureDir::create("finish-now");
    let source = LiveSource::start(&ndi, "finish-now");
    let file = scratch.join("take.mkv");
    let mut recorder = crate::file::FileRecorder::new(&api, &exact_video(&source)).unwrap();
    recorder
        .start(
            &RecordingTarget::new(&file).unwrap(),
            RecordingTimecode::Preserve,
        )
        .unwrap();
    await_event(&mut recorder, RecorderEventKind::FirstFrame);
    thread::sleep(Duration::from_millis(200));
    // The 6.4 library finalizes the file inside the stop call and queues its
    // close before returning, so no wait is needed to see it.
    let outcome = recorder.finish(Duration::ZERO).unwrap();
    let FinishStatus::Ended(SessionEnd::Closed(report)) = outcome else {
        panic!("the close was already queued: {outcome:?}");
    };
    assert!(report.closure_confirmed());
    assert!(!report.stopped_by_sdk());
    assert!(crate::file::FilePlayer::probe(&api, &file)
        .unwrap()
        .duration()
        .is_final());
}
