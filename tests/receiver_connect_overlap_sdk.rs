//! Native connect/capture overlap, including the SDK's synchronizers and remote
//! connection path. Each executable runs inside the CI loopback-only runner.
//!
//! `GRAFTON_NDI_OVERLAP_SECONDS=120`, `GRAFTON_NDI_OVERLAP_THREADS=16` and
//! `GRAFTON_NDI_OVERLAP_HD=1` select the longer 1080p evidence workload. Captures
//! and connection changes may overlap in the production receiver, so this
//! exercises the same native combination as ordinary applications.

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
#[cfg(any(feature = "advanced", all(target_os = "linux", not(ndi_link_static))))]
common_helpers!(runtime, prerequisites, name, loopback, discovery_server);
#[cfg(not(any(feature = "advanced", all(target_os = "linux", not(ndi_link_static)))))]
common_helpers!(runtime, name, loopback);

#[cfg(all(target_os = "linux", not(ndi_link_static)))]
#[path = "support/fixture.rs"]
mod fixture;

use grafton_ndi::{
    AudioFrame, AudioSource, Error, FrameRate, FrameSync, FrameSyncAudioRequest, MetadataFrame,
    Ndi, PixelFormat, Receiver, ReceiverItemRef, ReceiverMode, ReceiverOptions, ScanType, Sender,
    SenderOptions, Source, Timecode, VideoFrame, VideoSource,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant},
};

const SLICE: Duration = Duration::from_millis(100);
const PATIENCE: Duration = Duration::from_secs(15);

fn sockets() -> MutexGuard<'static, ()> {
    static SOCKETS: Mutex<()> = Mutex::new(());
    SOCKETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn duration() -> Duration {
    Duration::from_secs(
        std::env::var("GRAFTON_NDI_OVERLAP_SECONDS")
            .map(|value| value.parse().unwrap())
            .unwrap_or(2),
    )
}

fn margin() -> Duration {
    Duration::from_secs(
        std::env::var("GRAFTON_NDI_OVERLAP_MARGIN_SECONDS")
            .map(|value| value.parse().unwrap())
            .unwrap_or(1),
    )
}

fn timed<T>(operation: &str, budget: Duration, call: impl FnOnce() -> T) -> T {
    let began = Instant::now();
    let result = call();
    assert!(
        began.elapsed() <= budget + margin(),
        "{operation} exceeded its budget: {:?}",
        began.elapsed()
    );
    result
}

struct Stop<'a>(&'a AtomicBool);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Peers {
    senders: [Sender; 2],
    sources: [Source; 2],
    width: u32,
    height: u32,
}

/// Count complete feed iterations and upstream messages, including on a failed
/// connection-progress assertion after the scoped workers have stopped.
#[derive(Default)]
struct PeerTraffic {
    sent: [AtomicU64; 2],
    upstream: [AtomicU64; 2],
}

impl Drop for PeerTraffic {
    fn drop(&mut self) {
        for index in 0..2 {
            eprintln!(
                "peer {index}: {} video/audio/metadata groups sent, {} upstream messages handled",
                self.sent[index].load(Ordering::Relaxed),
                self.upstream[index].load(Ordering::Relaxed),
            );
        }
    }
}

impl Peers {
    fn new(ndi: &Ndi) -> Self {
        let senders = ["overlap BGRA", "overlap UYVY"].map(|name| {
            let label = format!("{name} {}", thread::current().name().unwrap_or("probe"));
            Sender::new(
                ndi,
                &SenderOptions::new(common::unique_name(&label))
                    .clock_video(true)
                    .clock_audio(false),
            )
            .unwrap()
        });
        let sources = senders
            .each_ref()
            .map(|sender| common::loopback_source(ndi, sender).unwrap());
        let (width, height) = if std::env::var_os("GRAFTON_NDI_OVERLAP_HD").is_some() {
            (1920, 1080)
        } else {
            (320, 180)
        };
        Self {
            senders,
            sources,
            width,
            height,
        }
    }

    fn feed(&self, index: usize, stopped: &AtomicBool, sent: &AtomicU64) {
        let format = if index == 0 {
            PixelFormat::Bgra
        } else {
            PixelFormat::Uyvy
        };
        // Reuse twelve prepared gray levels. Filling every HD pixel in Rust
        // for every send would dominate a detector run before the SDK could
        // deliver the video's following audio packet.
        let mut videos: [VideoFrame; 12] = std::array::from_fn(|phase| {
            let level = 16 + phase as u8 * 16;
            let count = self.width as usize * self.height as usize;
            let pixels = if index == 0 {
                [level, level, level, 255].repeat(count)
            } else {
                [128, level, 128, level].repeat(count / 2)
            };
            VideoFrame::builder()
                .resolution(self.width, self.height)
                .pixel_format(format)
                .frame_rate(FrameRate::new(60, 1).unwrap())
                .data(pixels)
                .build()
                .unwrap()
        });
        let audio = AudioFrame::builder()
            .sample_rate(48_000)
            .num_channels(2)
            .num_samples(800)
            .data(vec![if index == 0 { 0.125 } else { -0.125 }; 1600])
            .build()
            .unwrap();
        let metadata = MetadataFrame::new("<overlap/>").unwrap();
        for sequence in 0i64.. {
            if stopped.load(Ordering::Acquire) {
                break;
            }
            let video = &mut videos[sequence as usize % videos.len()];
            video.set_timecode(Timecode::from_ticks(sequence * 2 + index as i64));
            self.senders[index].send_video(video).unwrap();
            self.senders[index].send_audio(&audio);
            self.senders[index].send_metadata(&metadata);
            sent.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn run(&self, work: impl FnOnce(&AtomicBool)) -> u64 {
        let stopped = AtomicBool::new(false);
        let traffic = PeerTraffic::default();
        thread::scope(|scope| {
            for index in 0..2 {
                let stopped = &stopped;
                let sent = &traffic.sent[index];
                scope.spawn(move || self.feed(index, stopped, sent));
                let upstream = &traffic.upstream[index];
                // PTZ, connection metadata and explicit metadata travel back
                // to the source. Service that direction independently: a
                // synchronous media send must not hold up its own consumer.
                scope.spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        if let Some(message) = timed("sender upstream metadata", SLICE, || {
                            self.senders[index].capture_metadata_ref(SLICE)
                        })
                        .unwrap()
                        {
                            upstream.fetch_add(1, Ordering::Relaxed);
                            drop(message);
                        }
                    }
                });
            }
            let _stop = Stop(&stopped);
            work(&stopped);
        });
        traffic
            .upstream
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .sum()
    }
}

fn check_video(frame: &impl VideoSource, peers: &Peers) {
    assert_eq!((frame.width(), frame.height()), (peers.width, peers.height));
    assert!(matches!(
        frame.pixel_format(),
        PixelFormat::Bgra | PixelFormat::Bgrx
    ));
    let sequence = frame.timecode().ticks();
    assert!(sequence >= 0, "missing sequence timecode");
    let level = 16 + (sequence / 2 % 12) as u8 * 16;
    let expected = if sequence % 2 == 0 {
        level
    } else {
        ((u32::from(level) - 16) * 255 / 219) as u8
    };
    for plane in frame.planes() {
        for row in plane.rows() {
            assert!(
                row.as_chunks::<4>().0.iter().all(|pixel| {
                    pixel[..3]
                        .iter()
                        .all(|&value| value.abs_diff(expected) <= 8)
                }),
                "torn picture or sequence mismatch, sequence={sequence}, expected={expected}"
            );
        }
    }
}

fn check_audio(frame: &impl AudioSource) {
    assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
    assert!(frame
        .channels()
        .flatten()
        .all(|value| value.is_finite() && value.abs() <= 1.0));
}

#[derive(Default)]
struct Loans {
    taken: AtomicU64,
    returned: AtomicU64,
    videos: AtomicU64,
    audio: AtomicU64,
}

impl Loans {
    fn check<T>(&self, frame: T, validate: impl FnOnce(&T)) {
        self.taken.fetch_add(1, Ordering::Relaxed);
        validate(&frame);
        // Native borrowed-frame Drop must finish before the returned count.
        drop(frame);
        self.returned.fetch_add(1, Ordering::Relaxed);
    }

    fn video<T: VideoSource>(&self, frame: T, peers: &Peers) {
        self.check(frame, |frame| check_video(frame, peers));
        self.videos.fetch_add(1, Ordering::Relaxed);
    }

    fn audio<T: AudioSource>(&self, frame: T) {
        self.check(frame, check_audio);
        self.audio.fetch_add(1, Ordering::Relaxed);
    }

    fn ready(&self) -> bool {
        self.videos.load(Ordering::Relaxed) > 0 && self.audio.load(Ordering::Relaxed) > 0
    }

    fn wait_for_media(&self, ndi: &Ndi) {
        let deadline = Instant::now() + PATIENCE + margin();
        while !self.ready() {
            assert!(
                Instant::now() < deadline,
                "video/audio did not warm up: {:?}",
                ndi.info()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn balanced(&self, ndi: &Ndi) {
        assert_eq!(
            self.taken.load(Ordering::Relaxed),
            self.returned.load(Ordering::Relaxed)
        );
        assert!(
            self.videos.load(Ordering::Relaxed) > 0,
            "no video: {:?}",
            ndi.info()
        );
        assert!(
            self.audio.load(Ordering::Relaxed) > 0,
            "no audio: {:?}",
            ndi.info()
        );
        eprintln!(
            "overlap loans: {} video, {} audio, {} returned; runtime {:?}",
            self.videos.load(Ordering::Relaxed),
            self.audio.load(Ordering::Relaxed),
            self.returned.load(Ordering::Relaxed),
            ndi.info()
        );
    }
}

fn captured<T>(result: Result<Option<T>, Error>) -> Option<T> {
    match result {
        Ok(frame) => frame,
        Err(Error::ConnectionLost) => None,
        Err(error) => panic!("unexpected capture refusal during connect: {error:?}"),
    }
}

fn progress<M: ReceiverMode>(receiver: &Receiver<M>) {
    progress_after_in(receiver, 0, "initial video progress");
}

fn progress_after_in<M: ReceiverMode>(receiver: &Receiver<M>, before: u64, phase: &str) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let stats = receiver.connection_stats().unwrap();
        let received = stats.video_frames_received();
        if received > 0 && received != before {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no progress during {phase}; video count before={before}; target {:?}; stats {:?}",
            receiver.connection_target(),
            stats,
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn storm<M: ReceiverMode>(receiver: &Receiver<M>, peers: &Peers, stopped: &AtomicBool) {
    let deadline = Instant::now() + duration();
    let mut round = 0u64;
    while Instant::now() < deadline && !stopped.load(Ordering::Acquire) {
        for (index, source) in peers.sources.iter().enumerate() {
            let before = receiver.connection_stats().unwrap();
            let phase = format!("round {round} connect_to peer {index} (before: {before:?})");
            timed(&phase, Duration::ZERO, || receiver.connect_to(source));
            progress_after_in(receiver, before.video_frames_received(), &phase);
            thread::sleep(Duration::from_millis(round % 21));
        }
        timed("disconnect", Duration::ZERO, || receiver.disconnect());
        thread::sleep(Duration::from_millis(round % 21));
        let before = receiver.connection_stats().unwrap();
        let phase = format!("round {round} reconnect (before: {before:?})");
        timed(&phase, Duration::ZERO, || receiver.reconnect());
        progress_after_in(receiver, before.video_frames_received(), &phase);
        round += 1;
    }
    let before = receiver.connection_stats().unwrap();
    let phase = format!("final connect_to peer 0 after {round} rounds (before: {before:?})");
    receiver.connect_to(&peers.sources[0]);
    progress_after_in(receiver, before.video_frames_received(), &phase);
    eprintln!("completed {round} connect/disconnect/reconnect rounds");
}

#[test]
fn native_capture_and_connect_overlap_without_torn_frames() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap();
    let loans = Loans::default();
    let workers = std::env::var("GRAFTON_NDI_OVERLAP_THREADS")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(3);
    assert!(workers >= 3);
    peers.run(|stopped| {
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            for kind in 0..workers {
                let (receiver, peers, loans) = (&receiver, &peers, &loans);
                scope.spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        match kind % 3 {
                            0 => {
                                if let Some(frame) = captured(timed("video capture", SLICE, || {
                                    receiver.capture_video_ref(SLICE)
                                })) {
                                    loans.video(frame, peers);
                                }
                            }
                            1 => {
                                if let Some(frame) = captured(timed("audio capture", SLICE, || {
                                    receiver.capture_audio_ref(SLICE)
                                })) {
                                    loans.audio(frame);
                                }
                            }
                            _ => match captured(timed("mixed capture", SLICE, || {
                                receiver.capture_any_ref(SLICE)
                            })) {
                                Some(ReceiverItemRef::Video(frame)) => loans.video(frame, peers),
                                Some(ReceiverItemRef::Audio(frame)) => loans.audio(frame),
                                Some(ReceiverItemRef::Metadata(frame)) => loans
                                    .check(frame, |frame| assert_eq!(frame.data(), "<overlap/>")),
                                _ => {}
                            },
                        }
                    }
                });
            }
            progress(&receiver);
            loans.wait_for_media(&ndi);
            storm(&receiver, &peers, stopped);
        });
    });
    loans.balanced(&ndi);
}

// Refusals caused by an absent source are part of the control contract.
// PTZ and KVM report NativeOperationFailed when their message was not sent.
fn control_result<T: std::fmt::Debug>(result: Result<T, Error>) {
    assert!(
        matches!(
            result,
            Ok(_) | Err(Error::NotConnected | Error::NativeOperationFailed { .. })
        ),
        "unexpected shared-control refusal: {result:?}"
    );
}

fn shared_controls<M: ReceiverMode>(receiver: &Receiver<M>, kind: usize, round: u64) {
    use grafton_ndi::Tally;
    match kind % 3 {
        0 => {
            let ptz = receiver.ptz();
            let _ = ptz.is_supported();
            control_result(ptz.recall_preset(0, 0.5));
            control_result(ptz.store_preset(0));
            control_result(ptz.zoom(0.5));
            control_result(ptz.zoom_speed(0.0));
            control_result(ptz.pan_tilt(0.0, 0.0));
            control_result(ptz.pan_tilt_speed(0.0, 0.0));
            control_result(ptz.auto_focus());
            control_result(ptz.focus(0.5));
            control_result(ptz.focus_speed(0.0));
            control_result(ptz.white_balance_auto());
            control_result(ptz.white_balance_indoor());
            control_result(ptz.white_balance_outdoor());
            control_result(ptz.white_balance_oneshot());
            control_result(ptz.white_balance_manual(0.5, 0.5));
            control_result(ptz.exposure_auto());
            control_result(ptz.exposure_manual(0.5));
            control_result(ptz.exposure_manual_detailed(1.0, 0.5, 0.5));
        }
        1 => {
            receiver.set_tally(Tally::new(
                round.is_multiple_of(2),
                !round.is_multiple_of(2),
            ));
            let metadata = MetadataFrame::new("<control-overlap/>").unwrap();
            control_result(receiver.send_metadata(&metadata));
            control_result(receiver.add_connection_metadata(&metadata));
            control_result(receiver.clear_connection_metadata());
            control_result(receiver.web_control_url());
        }
        _ => {
            control_result(receiver.connection_stats());
            let _ = receiver.is_connected();
            // The capability was selected once by the owner before spawning.
            // Older runtimes may lack the delay or keyframe families entirely.
            for result in [
                receiver.set_video_delay(Duration::from_millis(round % 2)),
                receiver.set_audio_delay(Duration::from_millis(round % 2)),
            ] {
                if !matches!(result, Err(Error::UnsupportedCapability { .. })) {
                    control_result(result);
                }
            }
            for result in [receiver.video_delay(), receiver.audio_delay()] {
                if !matches!(result, Err(Error::UnsupportedCapability { .. })) {
                    control_result(result);
                }
            }
            #[cfg(feature = "advanced")]
            {
                let result = receiver.request_keyframe();
                if !matches!(result, Err(Error::UnsupportedCapability { .. })) {
                    control_result(result);
                }
            }
        }
    }
}

const CONTROL_FAMILIES: [&str; 3] = [
    "PTZ",
    "tally/metadata/registration/web",
    "delay/statistics/keyframe",
];

#[derive(Default)]
struct ControlProgress([AtomicU64; 3]);

impl Drop for ControlProgress {
    fn drop(&mut self) {
        for (family, count) in CONTROL_FAMILIES.iter().zip(&self.0) {
            eprintln!(
                "{family}: {} completed control groups",
                count.load(Ordering::Relaxed)
            );
        }
    }
}

#[test]
fn ptz_controls_capture_and_connect_complete_without_torn_frames() {
    shared_control_overlap(&[0]);
}

#[test]
fn metadata_controls_capture_and_connect_complete_without_torn_frames() {
    shared_control_overlap(&[1]);
}

#[test]
fn delay_and_statistics_capture_and_connect_complete_without_torn_frames() {
    shared_control_overlap(&[2]);
}

#[test]
fn shared_controls_capture_and_connect_complete_without_torn_frames() {
    shared_control_overlap(&[0, 1, 2]);
}

fn shared_control_overlap(kinds: &[usize]) {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap();
    let loans = Loans::default();
    let controls = ControlProgress::default();
    eprintln!(
        "control overlap runtime {:?}; peers {:?}",
        ndi.info(),
        peers.sources
    );
    let upstream = peers.run(|stopped| {
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            for &kind in kinds {
                let (receiver, controls) = (&receiver, &controls.0[kind]);
                scope.spawn(move || {
                    let mut round = 0;
                    while !stopped.load(Ordering::Acquire) {
                        timed("shared controls", Duration::ZERO, || {
                            shared_controls(receiver, kind, round)
                        });
                        controls.fetch_add(1, Ordering::Relaxed);
                        round += 1;
                        thread::sleep(Duration::from_millis(2));
                    }
                });
            }
            let workers = std::env::var("GRAFTON_NDI_OVERLAP_THREADS")
                .map(|value| value.parse::<usize>().unwrap())
                .unwrap_or(3);
            for kind in 0..workers {
                let (receiver, peers, loans) = (&receiver, &peers, &loans);
                scope.spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        match kind % 3 {
                            0 => {
                                if let Some(frame) =
                                    captured(timed("control overlap video", SLICE, || {
                                        receiver.capture_video_ref(SLICE)
                                    }))
                                {
                                    loans.video(frame, peers);
                                }
                            }
                            1 => {
                                if let Some(frame) =
                                    captured(timed("control overlap audio", SLICE, || {
                                        receiver.capture_audio_ref(SLICE)
                                    }))
                                {
                                    loans.audio(frame);
                                }
                            }
                            _ => match captured(timed("control overlap mixed", SLICE, || {
                                receiver.capture_any_ref(SLICE)
                            })) {
                                Some(ReceiverItemRef::Video(frame)) => loans.video(frame, peers),
                                Some(ReceiverItemRef::Audio(frame)) => loans.audio(frame),
                                _ => {}
                            },
                        }
                    }
                });
            }
            progress(&receiver);
            loans.wait_for_media(&ndi);
            storm(&receiver, &peers, stopped);
        });
    });
    for &kind in kinds {
        assert!(
            controls.0[kind].load(Ordering::Relaxed) > 0,
            "{} made no calls",
            CONTROL_FAMILIES[kind]
        );
    }
    if kinds.contains(&0) || kinds.contains(&1) {
        assert!(upstream > 0, "source handled no upstream controls");
    }
    loans.balanced(&ndi);
}

#[cfg(feature = "advanced")]
fn kvm_overlap(
    receiver: &mut Receiver,
    peers: &Peers,
    ndi: &Ndi,
    changes: impl FnOnce(&AtomicBool) + Send,
) {
    use grafton_ndi::advanced::{MouseButton, WheelAxis};
    let loans = Loans::default();
    peers.run(|stopped| {
        progress(receiver);
        let mut session = receiver.kvm().unwrap();
        let view = session.receiver();
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            scope.spawn(|| changes(stopped));
            scope.spawn(|| {
                while !stopped.load(Ordering::Acquire) {
                    match captured(timed("KVM overlap capture", SLICE, || {
                        view.capture_any_ref(SLICE)
                    })) {
                        Some(ReceiverItemRef::Video(frame)) => loans.video(frame, peers),
                        Some(ReceiverItemRef::Audio(frame)) => loans.audio(frame),
                        _ => {}
                    }
                }
            });
            loans.wait_for_media(ndi);
            let deadline = Instant::now() + duration();
            let mut groups = 0;
            while Instant::now() < deadline {
                timed("KVM controls", Duration::ZERO, || {
                    let _ = session.is_supported();
                    for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
                        control_result(session.mouse_down(button));
                        control_result(session.mouse_up(button));
                    }
                    for axis in [WheelAxis::Vertical, WheelAxis::Horizontal] {
                        control_result(session.wheel(axis, 0.5));
                    }
                    control_result(session.move_pointer(0.5, 0.5));
                    control_result(session.set_clipboard_text("overlap"));
                    control_result(session.key_down(0xff0d));
                    control_result(session.key_up(0xff0d));
                    control_result(session.touch(&[[0.2, 0.3]]));
                    control_result(session.finish_touch());
                    control_result(session.release_all());
                    view.set_tally(grafton_ndi::Tally::new(false, true));
                    control_result(view.request_keyframe());
                    control_result(view.connection_stats());
                });
                groups += 1;
                thread::sleep(Duration::from_millis(2));
            }
            eprintln!("completed {groups} KVM control groups");
        });
    });
    loans.balanced(ndi);
}

#[cfg(feature = "advanced")]
#[test]
fn kvm_controls_and_shared_capture_complete_without_torn_frames() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    if let Err(error) = ndi.require_feature(grafton_ndi::RuntimeFeature::Kvm) {
        common::skip(common::Requirement::Advanced, &error.to_string());
        return;
    }
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let mut receiver =
        Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap();
    kvm_overlap(&mut receiver, &peers, &ndi, |_| {});
}

#[test]
fn framesync_capture_and_connect_overlap_without_torn_frames() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let sync = FrameSync::new(
        Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap(),
    )
    .unwrap();
    let loans = Loans::default();
    peers.run(|stopped| {
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            scope.spawn(|| {
                while !stopped.load(Ordering::Acquire) {
                    if let Some(frame) = timed("FrameSync video", Duration::ZERO, || {
                        sync.capture_video_ref(ScanType::Progressive)
                    })
                    .unwrap()
                    {
                        loans.video(frame, &peers);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            });
            scope.spawn(|| {
                let request = FrameSyncAudioRequest::capture_at(48_000, 2, 800).unwrap();
                while !stopped.load(Ordering::Acquire) {
                    let frame = timed("FrameSync audio", Duration::ZERO, || {
                        sync.capture_audio_ref(request)
                    })
                    .unwrap();
                    loans.audio(frame);
                    thread::sleep(Duration::from_millis(2));
                }
            });
            progress(sync.receiver());
            loans.wait_for_media(&ndi);
            storm(sync.receiver(), &peers, stopped);
        });
    });
    loans.balanced(&ndi);
}

#[cfg(feature = "advanced")]
#[test]
fn avsync_capture_and_connect_overlap_without_invalid_audio() {
    use grafton_ndi::advanced::{AvSync, AvSyncOutcome, AvSyncRequest, AvSyncVideo};
    let Some(ndi) = common::ndi() else {
        return;
    };
    if let Err(error) = ndi.require_feature(grafton_ndi::RuntimeFeature::AvSync) {
        common::skip(common::Requirement::Advanced, &error.to_string());
        return;
    }
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let sync =
        AvSync::new(Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap())
            .unwrap();
    let loans = Loans::default();
    peers.run(|stopped| {
        let receiver = sync.receiver();
        let receive = || {
            let Some(video) = captured(timed("AV video capture", SLICE, || {
                receiver.capture_video_ref(SLICE)
            })) else {
                return;
            };
            check_video(&video, &peers);
            if let Some(stamped) = AvSyncVideo::new(&video) {
                if let AvSyncOutcome::Audio { frame, .. } =
                    timed("AV synchronization", Duration::ZERO, || {
                        sync.synchronize(stamped, AvSyncRequest::automatic())
                    })
                    .unwrap()
                {
                    loans.audio(frame);
                }
            }
            loans.video(video, &peers);
        };
        let warmup = Instant::now() + PATIENCE + margin();
        while !loans.ready() {
            receive();
            assert!(
                Instant::now() < warmup,
                "AV media did not warm up: {:?}",
                ndi.info()
            );
        }
        let finished = AtomicBool::new(false);
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            scope.spawn(|| {
                let _finished = Stop(&finished);
                storm(receiver, &peers, stopped);
            });
            while !finished.load(Ordering::Acquire) {
                receive();
            }
        });
    });
    loans.balanced(&ndi);
}

#[cfg(all(target_os = "linux", not(ndi_link_static)))]
#[test]
fn discovery_server_connects_overlap_capture_and_report_the_remote_target() {
    use grafton_ndi::{
        discovery_server::{
            AdvertiserOptions, ListenerOptions, RecvAdvertisement, RecvAdvertiser, RecvListener,
            SendAdvertisement, SendAdvertiser,
        },
        RuntimeFeature, SourceNameChange,
    };
    // The child shares this test executable's network namespace. Hold the
    // parent's turn across its lifetime, so other parent cases do not compete
    // with its discovery/transport setup. The child has its own process mutex.
    let _turn = sockets();
    // Remote commands carry names, not the loopback endpoint we give local
    // connections. Its native internal finder needs a loopback discovery hint,
    // installed before the child initializes the SDK's process configuration.
    if std::env::var_os("GRAFTON_NDI_OVERLAP_REMOTE_CHILD").is_none() {
        let fixture = fixture::FixtureDir::create("overlap-remote");
        std::fs::write(
            fixture.path().join("ndi-config.v1.json"),
            r#"{"ndi":{"networks":{"ips":"127.0.0.1"}}}"#,
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery_server_connects_overlap_capture_and_report_the_remote_target",
                "--nocapture",
            ])
            .env("GRAFTON_NDI_OVERLAP_REMOTE_CHILD", "1")
            .env("NDI_CONFIG_DIR", fixture.path())
            .output()
            .unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        eprint!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "remote overlap subprocess failed");
        return;
    }
    let Some(ndi) = common::ndi() else {
        return;
    };
    for feature in [
        RuntimeFeature::ReceiverAdvertising,
        RuntimeFeature::ReceiverDiscovery,
        RuntimeFeature::SenderAdvertising,
        RuntimeFeature::ConnectedSourceName,
    ] {
        if let Err(error) = ndi.require_feature(feature) {
            common::skip(common::Requirement::DiscoveryServer, &error.to_string());
            return;
        }
    }
    let Some(binary) = common::discovery_server::server_binary(&ndi) else {
        common::skip(
            common::Requirement::DiscoveryServer,
            "the selected SDK has no Discovery Server",
        );
        return;
    };
    let server = common::discovery_server::Server::start(&binary);
    let peers = Peers::new(&ndi);
    let options = AdvertiserOptions::new().servers([server.endpoint()]);
    let mut send_advertiser = SendAdvertiser::new(&ndi, &options).unwrap();
    for sender in &peers.senders {
        send_advertiser
            .register(sender, &SendAdvertisement::new())
            .unwrap();
    }
    let receiver_name = common::unique_name("overlap remote receiver");
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(peers.sources[0].clone()).name(&receiver_name),
    )
    .unwrap();
    let mut advertiser = RecvAdvertiser::new(&ndi, &options).unwrap();
    let _registration = advertiser
        .register(
            &receiver,
            &RecvAdvertisement::new()
                .allow_monitoring(true)
                .allow_controlling(true),
        )
        .unwrap();
    let mut listener =
        RecvListener::new(&ndi, &ListenerOptions::new().server(server.endpoint())).unwrap();
    let deadline = Instant::now() + common::discovery_server::SETTLE;
    let suffix = format!("({receiver_name})");
    let id = loop {
        listener.wait_for_change(SLICE).unwrap();
        if let Some(advertised) =
            listener
                .current_receivers()
                .unwrap()
                .into_iter()
                .find(|advertised| {
                    advertised
                        .name()
                        .is_some_and(|name| name.ends_with(&suffix))
                })
        {
            break advertised.id().unwrap().clone();
        }
        assert!(
            Instant::now() < deadline,
            "receiver did not register: {:?}",
            ndi.info()
        );
    };
    // Do not unsubscribe while a connect can be running: the discovery module
    // records an SDK unsubscribe/connect deadlock. Listener destruction happens
    // only after every capture/control worker below has joined.
    listener.subscribe(&id);
    #[cfg(feature = "advanced")]
    let mut receiver = receiver;
    #[cfg(feature = "advanced")]
    if ndi.require_feature(RuntimeFeature::Kvm).is_ok() {
        kvm_overlap(&mut receiver, &peers, &ndi, |stopped| {
            while !stopped.load(Ordering::Acquire) {
                listener
                    .request_connect(&id, peers.sources[1].name())
                    .unwrap();
                thread::sleep(Duration::from_millis(25));
                listener.request_disconnect(&id).unwrap();
                thread::sleep(Duration::from_millis(25));
                listener
                    .request_connect(&id, peers.sources[0].name())
                    .unwrap();
                thread::sleep(Duration::from_millis(250));
            }
        });
    }
    let loans = Loans::default();
    peers.run(|stopped| {
        thread::scope(|scope| {
            let _stop = Stop(stopped);
            for kind in 0..3 {
                let receiver = &receiver;
                scope.spawn(move || {
                    let mut round = 0;
                    while !stopped.load(Ordering::Acquire) {
                        timed("remote shared controls", Duration::ZERO, || {
                            shared_controls(receiver, kind, round)
                        });
                        round += 1;
                        thread::sleep(Duration::from_millis(2));
                    }
                });
            }
            scope.spawn(|| {
                while !stopped.load(Ordering::Acquire) {
                    match captured(timed("remote mixed capture", SLICE, || {
                        receiver.capture_any_ref(SLICE)
                    })) {
                        Some(ReceiverItemRef::Video(frame)) => loans.video(frame, &peers),
                        Some(ReceiverItemRef::Audio(frame)) => loans.audio(frame),
                        Some(ReceiverItemRef::Metadata(frame)) => loans.check(frame, |_| {}),
                        _ => {}
                    }
                }
            });
            let observe = |expected: SourceNameChange| {
                let deadline = Instant::now() + PATIENCE;
                loop {
                    let change = timed("source name change", SLICE, || {
                        receiver.wait_for_source_name_change(SLICE)
                    })
                    .unwrap();
                    if change.as_ref() == Some(&expected) {
                        return;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "remote target {expected:?} was not reported: {:?}",
                        ndi.info()
                    );
                }
            };
            for index in [0, 1, 0] {
                let before = receiver.connection_stats().unwrap().video_frames_received();
                listener
                    .request_connect(&id, peers.sources[index].name())
                    .unwrap();
                observe(SourceNameChange::Connected(
                    peers.sources[index].name().to_owned(),
                ));
                progress_after_in(&receiver, before, "remote request_connect");
                loans.wait_for_media(&ndi);
                assert_eq!(
                    receiver.connection_target(),
                    peers.sources[0],
                    "remote control changed the locally requested target"
                );
                listener.request_disconnect(&id).unwrap();
                observe(SourceNameChange::Disconnected);
            }
            receiver.reconnect();
            observe(SourceNameChange::Connected(
                peers.sources[0].name().to_owned(),
            ));
            progress(&receiver);
            // Remote requests also race local changes. Exact target assertions
            // above use settled phases, since a later local request is allowed
            // to replace a remote one before it can be observed.
            scope.spawn(|| {
                while !stopped.load(Ordering::Acquire) {
                    listener
                        .request_connect(&id, peers.sources[1].name())
                        .unwrap();
                    thread::sleep(Duration::from_millis(10));
                    listener.request_disconnect(&id).unwrap();
                    thread::sleep(Duration::from_millis(250));
                }
            });
            storm(&receiver, &peers, stopped);
        });
    });
    loans.balanced(&ndi);
}
