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

    fn feed(&self, index: usize, stopped: &AtomicBool, sent: &AtomicU64, stamp_audio: bool) {
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
        let mut audio = AudioFrame::builder()
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
            if stamp_audio {
                // Match video's source/sequence epoch for the quiet recovery
                // checks; strict capture/synchronizer fixtures keep synthesis.
                audio.set_timecode(Timecode::from_ticks(sequence * 2 + index as i64));
            }
            self.senders[index].send_audio(&audio);
            self.senders[index].send_metadata(&metadata);
            sent.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn run(&self, work: impl FnOnce(&AtomicBool)) -> u64 {
        self.run_with_traffic(false, |stopped, _traffic| work(stopped))
    }

    fn run_with_traffic(
        &self,
        stamp_audio: bool,
        work: impl FnOnce(&AtomicBool, &PeerTraffic),
    ) -> u64 {
        let stopped = AtomicBool::new(false);
        let traffic = PeerTraffic::default();
        thread::scope(|scope| {
            for index in 0..2 {
                let stopped = &stopped;
                let sent = &traffic.sent[index];
                scope.spawn(move || self.feed(index, stopped, sent, stamp_audio));
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
            work(&stopped, &traffic);
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
    latest_video_timecode: AtomicU64,
}

impl Drop for Loans {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "capture progress at failure: {} actual video loans, {} actual audio loans, {} total loan checks, {} validated loans returned, latest video timecode {}",
                self.videos.load(Ordering::Relaxed),
                self.audio.load(Ordering::Relaxed),
                self.taken.load(Ordering::Relaxed),
                self.returned.load(Ordering::Relaxed),
                self.latest_video_timecode.load(Ordering::Relaxed),
            );
        }
    }
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
        let timecode = frame.timecode().ticks();
        self.check(frame, |frame| check_video(frame, peers));
        // check_video validated the fixture's nonnegative sequence timecode.
        self.latest_video_timecode
            .fetch_max(timecode as u64, Ordering::Relaxed);
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
    control_load_then_settle(&[0]);
}

#[test]
fn metadata_controls_capture_and_connect_complete_without_torn_frames() {
    control_load_then_settle(&[1]);
}

#[test]
fn delay_and_statistics_capture_and_connect_complete_without_torn_frames() {
    control_load_then_settle(&[2]);
}

#[test]
fn shared_controls_capture_and_connect_complete_without_torn_frames() {
    control_load_then_settle(&[0, 1, 2]);
}

/// Manual throughput/liveness measurement, stronger than the SDK request contract.
#[test]
#[ignore = "manual liveness characterization: continuous metadata pressure does not guarantee fresh media after every asynchronous switch"]
fn continuous_control_pressure_and_per_change_media_liveness() {
    characterize_per_change_control_liveness();
}

fn characterize_per_change_control_liveness() {
    let kinds = &[0, 1, 2];
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
    assert!(upstream > 0, "source handled no upstream controls");
    loans.balanced(&ndi);
}

// Connection methods submit asynchronous SDK requests. Concurrent controls do
// not promise uninterrupted media after every request: require bounded calls,
// valid delivered frames, and convergence after load stops instead. The manual
// characterization above retains the stronger per-change measurement.
fn control_load_then_settle(kinds: &[usize]) {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let _turn = sockets();
    let peers = Peers::new(&ndi);
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap();
    let loans = Loans::default();
    let controls = ControlProgress::default();
    let workers = std::env::var("GRAFTON_NDI_OVERLAP_THREADS")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(3);
    assert!(workers >= 3);
    let latest_final_video = AtomicU64::new(0);
    let latest_final_audio = AtomicU64::new(0);
    eprintln!("convergence runtime {:?}", ndi.info());
    let wait_until = |phase: &str, deadline: Instant, complete: &dyn Fn() -> bool| {
        while !complete() {
            assert!(
                Instant::now() < deadline,
                "{phase}: video {}, audio {}, stats {:?}",
                latest_final_video.load(Ordering::Relaxed),
                latest_final_audio.load(Ordering::Relaxed),
                receiver.connection_stats().unwrap()
            );
            thread::sleep(Duration::from_millis(2));
        }
    };
    let upstream = peers.run_with_traffic(true, |stopped, traffic| {
        thread::scope(|captures| {
            let _stop = Stop(stopped);
            for kind in 0..workers {
                let (receiver, peers, loans, latest_video, audio) = (
                    &receiver,
                    &peers,
                    &loans,
                    &latest_final_video,
                    &latest_final_audio,
                );
                captures.spawn(move || {
                    let video = |frame: grafton_ndi::VideoFrameRef<'_>| {
                        let timecode = frame.timecode().ticks();
                        loans.video(frame, peers);
                        if timecode % 2 == 1 {
                            latest_video.fetch_max(timecode as u64, Ordering::Relaxed);
                        }
                    };
                    let sound = |frame: grafton_ndi::AudioFrameRef<'_>| {
                        let timecode = frame.timecode().ticks();
                        let peer1 = frame
                            .channels()
                            .flatten()
                            .next()
                            .is_some_and(|v| v.is_sign_negative() && v.abs() > 0.0);
                        loans.audio(frame);
                        if peer1 {
                            assert!(
                                timecode >= 0 && timecode % 2 == 1,
                                "final-target audio lost its sequence timecode: {timecode}"
                            );
                            audio.fetch_max(timecode as u64, Ordering::Relaxed);
                        }
                    };
                    while !stopped.load(Ordering::Acquire) {
                        match kind % 3 {
                            0 => {
                                if let Some(frame) =
                                    captured(timed("convergence video", SLICE, || {
                                        receiver.capture_video_ref(SLICE)
                                    }))
                                {
                                    video(frame);
                                }
                            }
                            1 => {
                                if let Some(frame) =
                                    captured(timed("convergence audio", SLICE, || {
                                        receiver.capture_audio_ref(SLICE)
                                    }))
                                {
                                    sound(frame);
                                }
                            }
                            _ => match captured(timed("convergence mixed", SLICE, || {
                                receiver.capture_any_ref(SLICE)
                            })) {
                                Some(ReceiverItemRef::Video(frame)) => video(frame),
                                Some(ReceiverItemRef::Audio(frame)) => sound(frame),
                                _ => {}
                            },
                        }
                    }
                });
            }
            wait_until(
                "initial continuous media",
                Instant::now() + PATIENCE,
                &|| {
                    loans.videos.load(Ordering::Relaxed) >= 3
                        && loans.audio.load(Ordering::Relaxed) >= 3
                },
            );
            let stop_controls = AtomicBool::new(false);
            thread::scope(|control_scope| {
                let _stop_controls = Stop(&stop_controls);
                for &kind in kinds {
                    let (receiver, count, stopped) = (&receiver, &controls.0[kind], &stop_controls);
                    control_scope.spawn(move || {
                        let mut round = 0;
                        while !stopped.load(Ordering::Acquire) {
                            timed("convergence controls", Duration::ZERO, || {
                                shared_controls(receiver, kind, round)
                            });
                            count.fetch_add(1, Ordering::Relaxed);
                            round += 1;
                            thread::sleep(Duration::from_millis(2));
                        }
                    });
                }
                let steady_deadline = Instant::now() + PATIENCE;
                wait_until("controls start", steady_deadline, &|| {
                    kinds
                        .iter()
                        .all(|&kind| controls.0[kind].load(Ordering::Relaxed) > 0)
                });
                // Observe steady control/capture load before changing source.
                // A video pause under load must not skip the later recovery check.
                thread::sleep(SLICE);
                eprintln!(
                    "steady load before connection changes: video {}, audio {}, stats {:?}",
                    latest_final_video.load(Ordering::Relaxed),
                    latest_final_audio.load(Ordering::Relaxed),
                    receiver.connection_stats().unwrap()
                );
                let before_controls = controls
                    .0
                    .each_ref()
                    .map(|count| count.load(Ordering::Relaxed));
                let deadline = Instant::now() + duration();
                let mut round = 0;
                while Instant::now() < deadline {
                    for (index, source) in peers.sources.iter().enumerate() {
                        timed(
                            &format!("load round {round} connect_to peer {index}"),
                            Duration::ZERO,
                            || receiver.connect_to(source),
                        );
                        thread::sleep(Duration::from_millis(round % 21));
                    }
                    timed("load disconnect", Duration::ZERO, || receiver.disconnect());
                    thread::sleep(Duration::from_millis(round % 21));
                    timed("load reconnect", Duration::ZERO, || receiver.reconnect());
                    thread::sleep(Duration::from_millis(round % 21));
                    round += 1;
                }
                timed(
                    "final target request under control load",
                    Duration::ZERO,
                    || receiver.connect_to(&peers.sources[1]),
                );
                for &kind in kinds {
                    assert!(
                        controls.0[kind].load(Ordering::Relaxed) > before_controls[kind],
                        "{} made no calls during connection load",
                        CONTROL_FAMILIES[kind]
                    );
                }
                eprintln!(
                    "final peer1 target issued after {round} load rounds; stopping only controls"
                );
            });
            // No request follows this point. Feed count n completes sequence
            // n-1; at most one following synchronous video send is in progress.
            // Require a peer1 sequence >n+1 to exclude pre-stop queued video.
            let deadline = Instant::now() + PATIENCE;
            let cutoff = traffic.sent[1]
                .load(Ordering::Relaxed)
                .saturating_add(1)
                .saturating_mul(2)
                .saturating_add(1);
            wait_until(
                "fresh final-target video after controls stop",
                deadline,
                &|| latest_final_video.load(Ordering::Relaxed) > cutoff,
            );
            wait_until(
                "fresh final-target audio after controls stop",
                deadline,
                &|| latest_final_audio.load(Ordering::Relaxed) > cutoff,
            );
            eprintln!(
                "converged without another request: peer1 video {} > {cutoff}; audio {}",
                latest_final_video.load(Ordering::Relaxed),
                latest_final_audio.load(Ordering::Relaxed)
            );
        });
    });
    if kinds.iter().any(|&kind| kind == 0 || kind == 1) {
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

/// Membership add/remove owns an independent receiver reference. Churn its
/// entire lifecycle while captures, connection requests and controls run.
#[cfg(all(target_os = "linux", not(ndi_link_static)))]
#[test]
fn advertiser_membership_changes_overlap_capture_connect_and_controls() {
    use grafton_ndi::{
        discovery_server::{AdvertiserOptions, RecvAdvertisement, RecvAdvertiser},
        RuntimeFeature,
    };
    let Some(ndi) = common::ndi() else { return };
    if let Err(error) = ndi.require_feature(RuntimeFeature::ReceiverAdvertising) {
        common::skip(common::Requirement::DiscoveryServer, &error.to_string());
        return;
    }
    let Some(binary) = common::discovery_server::server_binary(&ndi) else {
        common::skip(
            common::Requirement::DiscoveryServer,
            "no vendor Discovery Server",
        );
        return;
    };
    let _turn = sockets();
    let server = common::discovery_server::Server::start(&binary);
    let options = AdvertiserOptions::new().servers([server.endpoint()]);
    let peers = Peers::new(&ndi);
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(peers.sources[0].clone())).unwrap();
    let loans = Loans::default();
    let memberships = AtomicU64::new(0);
    peers.run(|stopped| {
        thread::scope(|captures| {
            let _stop = Stop(stopped);
            let receiver = &receiver;
            captures.spawn(|| {
                while !stopped.load(Ordering::Acquire) {
                    match captured(timed("membership capture", SLICE, || {
                        receiver.capture_any_ref(SLICE)
                    })) {
                        Some(ReceiverItemRef::Video(frame)) => loans.video(frame, &peers),
                        Some(ReceiverItemRef::Audio(frame)) => loans.audio(frame),
                        Some(ReceiverItemRef::Metadata(frame)) => loans.check(frame, |_| {}),
                        _ => {}
                    }
                }
            });
            progress(receiver);
            loans.wait_for_media(&ndi);
            let loading = AtomicBool::new(true);
            thread::scope(|load| {
                load.spawn(|| {
                    while loading.load(Ordering::Acquire) {
                        let mut advertiser = RecvAdvertiser::new(&ndi, &options).unwrap();
                        let advertisement = RecvAdvertisement::new()
                            .allow_controlling(true)
                            .allow_monitoring(true);
                        let registration = timed("membership add", Duration::ZERO, || {
                            advertiser.register(receiver, &advertisement)
                        })
                        .unwrap();
                        timed("membership remove", Duration::ZERO, || {
                            advertiser.unregister(registration)
                        })
                        .unwrap();
                        advertiser.register(receiver, &advertisement).unwrap();
                        // Removes the second registration through advertiser Drop.
                        timed("membership drop", Duration::ZERO, || drop(advertiser));
                        memberships.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(2));
                    }
                });
                for kind in 0..3 {
                    let loading = &loading;
                    load.spawn(move || {
                        let mut round = 0;
                        while loading.load(Ordering::Acquire) {
                            timed("membership shared controls", Duration::ZERO, || {
                                shared_controls(receiver, kind, round)
                            });
                            round += 1;
                            thread::sleep(Duration::from_millis(2));
                        }
                    });
                }
                load.spawn(|| {
                    while loading.load(Ordering::Acquire) {
                        for source in &peers.sources {
                            timed("membership connect", Duration::ZERO, || {
                                receiver.connect_to(source)
                            });
                            thread::sleep(Duration::from_millis(10));
                        }
                        receiver.disconnect();
                        receiver.reconnect();
                    }
                });
                thread::sleep(duration());
                loading.store(false, Ordering::Release);
            });
            let before = receiver.connection_stats().unwrap().video_frames_received();
            receiver.connect_to(&peers.sources[0]);
            progress_after_in(receiver, before, "convergence after membership churn");
        });
    });
    loans.balanced(&ndi);
    assert!(memberships.load(Ordering::Relaxed) > 0);
    eprintln!(
        "completed {} advertiser lifecycle rounds on {:?}",
        memberships.load(Ordering::Relaxed),
        ndi.info()
    );

    // A prior registration can be removed on another thread during mutable
    // stream controls and KVM. Its independent Arc defeats &mut-only exclusion.
    #[cfg(feature = "advanced")]
    {
        let mut receiver = receiver;
        let mut advertiser = RecvAdvertiser::new(&ndi, &options).unwrap();
        advertiser
            .register(&receiver, &RecvAdvertisement::new())
            .unwrap();
        thread::scope(|scope| {
            scope.spawn(move || {
                thread::sleep(Duration::from_millis(1));
                drop(advertiser);
            });
            for _ in 0..16 {
                if ndi
                    .require_feature(RuntimeFeature::ReceiverStreamTier)
                    .is_ok()
                {
                    control_result(receiver.set_stream_tier(grafton_ndi::StreamTier::Program));
                }
                if ndi
                    .require_feature(RuntimeFeature::ReceiverStreamSelection)
                    .is_ok()
                {
                    control_result(receiver.set_streams(grafton_ndi::MediaTypes::ALL));
                }
            }
        });
        if ndi.require_feature(RuntimeFeature::Kvm).is_ok() {
            let mut advertiser = RecvAdvertiser::new(&ndi, &options).unwrap();
            advertiser
                .register(&receiver, &RecvAdvertisement::new())
                .unwrap();
            kvm_overlap(&mut receiver, &peers, &ndi, move |_| {
                thread::sleep(Duration::from_millis(50));
                drop(advertiser);
            });
        }
    }
}

/// Native connection changes can join the decoder thread running a policy, and
/// allocator reset waits on its native read lock. That policy must still be
/// able to add or remove membership through the independent observer registry.
#[cfg(all(feature = "advanced", target_os = "linux", not(ndi_link_static)))]
#[test]
fn receiver_mutation_allows_membership_changes_from_active_allocator_policies() {
    use grafton_ndi::{
        advanced::{AllocatedReceiver, AllocationBuffer, AllocatorLimits, AllocatorService},
        discovery_server::{AdvertiserOptions, RecvAdvertisement, RecvAdvertiser},
        RuntimeFeature,
    };
    use std::{
        process::{Command, Stdio},
        sync::{mpsc, Arc, OnceLock, Weak},
    };
    let Some(ndi) = common::ndi() else { return };
    for (feature, requirement) in [
        (
            RuntimeFeature::ReceiveAllocators,
            common::Requirement::Advanced,
        ),
        (
            RuntimeFeature::ReceiverAdvertising,
            common::Requirement::DiscoveryServer,
        ),
    ] {
        if let Err(error) = ndi.require_feature(feature) {
            common::skip(requirement, &error.to_string());
            return;
        }
    }
    let _turn = sockets();
    if std::env::var_os("GRAFTON_NDI_ALLOCATOR_MEMBERSHIP_CHILD").is_none() {
        for (operation, action) in [
            ("reset", "remove"),
            ("reconnect", "remove"),
            ("disconnect", "remove"),
            ("reconnect", "add"),
            ("disconnect", "add"),
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "receiver_mutation_allows_membership_changes_from_active_allocator_policies",
                    "--nocapture",
                ])
                .env("GRAFTON_NDI_ALLOCATOR_MEMBERSHIP_CHILD", "1")
                .env("GRAFTON_NDI_MEMBERSHIP_ACTION", action)
                .env("GRAFTON_NDI_NATIVE_OPERATION", operation)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(45);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "{operation}/{action} did not complete:\n{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{operation}/{action} subprocess failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let operation = std::env::var("GRAFTON_NDI_NATIVE_OPERATION").unwrap();
    let adding = std::env::var("GRAFTON_NDI_MEMBERSHIP_ACTION").unwrap() == "add";
    let Some(binary) = common::discovery_server::server_binary(&ndi) else {
        common::skip(
            common::Requirement::DiscoveryServer,
            "no vendor Discovery Server",
        );
        return;
    };
    let server = common::discovery_server::Server::start(&binary);
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("allocator membership"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source)).unwrap();
    let mut advertiser =
        RecvAdvertiser::new(&ndi, &AdvertiserOptions::new().servers([server.endpoint()])).unwrap();
    if !adding {
        advertiser
            .register(&receiver, &RecvAdvertisement::new())
            .unwrap();
    }
    let advertiser = Mutex::new(Some(advertiser));
    let entered = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let policy_finished = Arc::new(AtomicBool::new(false));
    let shared_owner = Arc::new(OnceLock::<Weak<AllocatedReceiver>>::new());
    let stopped = AtomicBool::new(false);
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut allocated = service.attach(receiver).unwrap();
    {
        let (entered, released, policy_finished, shared_owner) = (
            Arc::clone(&entered),
            Arc::clone(&released),
            Arc::clone(&policy_finished),
            Arc::clone(&shared_owner),
        );
        allocated
            .set_video_allocator(move |request| {
                if !entered.swap(true, Ordering::AcqRel) {
                    eprintln!("allocator policy entered");
                    while !released.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(1));
                    }
                    eprintln!("allocator policy changing membership");
                    let mut advertiser = advertiser.lock().unwrap().take().unwrap();
                    if adding {
                        // Safe shared access can be published after policy setup:
                        // the callback needs no mutable owner or application mutex.
                        let owner = shared_owner.get().unwrap().upgrade().unwrap();
                        advertiser
                            .register(owner.receiver(), &RecvAdvertisement::new())
                            .unwrap();
                    }
                    drop(advertiser);
                    eprintln!("allocator policy changed membership");
                    policy_finished.store(true, Ordering::Release);
                }
                AllocationBuffer::zeroed(request)
            })
            .unwrap();
    }
    enum Owner {
        Exclusive(AllocatedReceiver),
        Shared(Arc<AllocatedReceiver>),
    }
    let owner = if operation == "reset" {
        Owner::Exclusive(allocated)
    } else {
        let owner = Arc::new(allocated);
        shared_owner.set(Arc::downgrade(&owner)).unwrap();
        Owner::Shared(owner)
    };
    thread::scope(|scope| {
        let _stop = Stop(&stopped);
        scope.spawn(|| {
            let frame = VideoFrame::builder()
                .resolution(64, 64)
                .data(vec![1; 64 * 64 * 4])
                .build()
                .unwrap();
            while !stopped.load(Ordering::Acquire) {
                sender.send_video(&frame).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        let receiver = match &owner {
            Owner::Exclusive(owner) => owner.receiver(),
            Owner::Shared(owner) => owner.receiver(),
        };
        let deadline = Instant::now() + PATIENCE;
        while !entered.load(Ordering::Acquire) {
            drop(receiver.capture_video_ref(Duration::ZERO));
            assert!(
                Instant::now() < deadline,
                "the allocator policy never started"
            );
            thread::sleep(Duration::from_millis(1));
        }
        // Capture has returned while a native background policy remains live.
        let (started, waiting) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let finished = Arc::clone(&policy_finished);
        let resetting = operation == "reset";
        scope.spawn(move || {
            let mut owner = owner;
            started.send(()).unwrap();
            match &mut owner {
                Owner::Exclusive(owner) => owner.reset_video_allocator(),
                Owner::Shared(owner) if operation == "reconnect" => owner.receiver().reconnect(),
                Owner::Shared(owner) => owner.receiver().disconnect(),
            }
            done.send(()).unwrap();
            // Retain the owner until the callback has changed membership, so
            // allocator cleanup on Drop cannot confound the operation's result.
            while !finished.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
        });
        waiting.recv_timeout(PATIENCE).unwrap();
        let premature = completed.recv_timeout(Duration::from_millis(100));
        released.store(true, Ordering::Release);
        if premature.is_err() {
            completed
                .recv_timeout(PATIENCE)
                .expect("native operation did not complete after the policy returned");
        }
        if resetting {
            assert!(
                premature.is_err(),
                "reset bypassed the still-running native policy"
            );
        }
        let deadline = Instant::now() + PATIENCE;
        while !policy_finished.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "policy could not change membership"
            );
            thread::sleep(Duration::from_millis(1));
        }
        stopped.store(true, Ordering::Release);
    });
    let deadline = Instant::now() + PATIENCE;
    loop {
        let usage = service.usage();
        if usage.generations() == 0
            && usage.retained_allocations() == 0
            && usage.in_progress_allocations() == 0
            && usage.in_progress_reclamations() == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "allocator grants/frees did not balance: {usage:?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Policies and external-buffer releases may use shared controls even while a
/// native connection change waits for that application code to return.
#[cfg(all(feature = "advanced", target_os = "linux", not(ndi_link_static)))]
#[test]
fn allocator_code_uses_shared_controls_during_connection_changes() {
    use grafton_ndi::{
        advanced::{
            AllocatedReceiver, AllocationBuffer, AllocatorLimits, AllocatorService,
            ExternalAllocation,
        },
        RuntimeFeature, Tally,
    };
    use std::{
        process::{Command, Stdio},
        ptr::NonNull,
        sync::{mpsc, Arc, OnceLock, Weak},
    };
    let Some(ndi) = common::ndi() else { return };
    if let Err(error) = ndi.require_feature(RuntimeFeature::ReceiveAllocators) {
        common::skip(common::Requirement::Advanced, &error.to_string());
        return;
    }
    let _turn = sockets();
    if std::env::var_os("GRAFTON_NDI_ALLOCATOR_CONTROLS_CHILD").is_none() {
        for operation in ["connect_to", "disconnect", "reconnect"] {
            for callback in ["policy", "release"] {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "allocator_code_uses_shared_controls_during_connection_changes",
                        "--nocapture",
                    ])
                    .env("GRAFTON_NDI_ALLOCATOR_CONTROLS_CHILD", "1")
                    .env("GRAFTON_NDI_NATIVE_OPERATION", operation)
                    .env("GRAFTON_NDI_ALLOCATOR_CALLBACK", callback)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(45);
                while child.try_wait().unwrap().is_none() {
                    if Instant::now() >= deadline {
                        child.kill().unwrap();
                        let output = child.wait_with_output().unwrap();
                        panic!(
                            "{operation}/{callback} did not complete:\n{}\n{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{operation}/{callback} failed:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                eprintln!("{operation}/{callback} completed");
            }
        }
        return;
    }

    fn controls(receiver: &Receiver) {
        receiver.set_tally(Tally::new(true, false));
        receiver.connection_stats().unwrap();
        let _ = receiver.web_control_url();
        let _ = receiver.video_delay();
        let _ = receiver.audio_delay();
        let metadata = MetadataFrame::new("<allocator-controls/>").unwrap();
        let _ = receiver.add_connection_metadata(&metadata);
        let _ = receiver.clear_connection_metadata();
        let _ = receiver.ptz().is_supported();
        let _ = receiver.request_keyframe();
        // KVM session creation requires an exclusive receiver borrow, which
        // allocator callbacks intentionally do not have. Its shared-control
        // permit is covered by the independent KVM overlap storm.
    }

    #[repr(align(64))]
    struct Block([u8; 64]);
    struct External {
        memory: Vec<Block>,
        owner: Arc<OnceLock<Weak<AllocatedReceiver>>>,
        armed: Arc<AtomicBool>,
        changing_thread: Arc<Mutex<Option<thread::ThreadId>>>,
        on_change: Arc<AtomicU64>,
    }
    // SAFETY: the vector owns initialized, aligned memory exclusively until
    // this handle drops; the SDK has relinquished it before Drop runs.
    unsafe impl ExternalAllocation for External {
        fn pointer(&self) -> NonNull<u8> {
            NonNull::from(&self.memory[0].0).cast()
        }
        fn capacity(&self) -> usize {
            self.memory.len() * 64
        }
    }
    impl Drop for External {
        fn drop(&mut self) {
            if self.armed.load(Ordering::Acquire) {
                let owner = self.owner.get().unwrap().upgrade().unwrap();
                controls(owner.receiver());
                if self.changing_thread.lock().unwrap().as_ref() == Some(&thread::current().id()) {
                    self.on_change.fetch_add(1, Ordering::Release);
                }
            }
        }
    }

    let operation = std::env::var("GRAFTON_NDI_NATIVE_OPERATION").unwrap();
    let policy_case = std::env::var("GRAFTON_NDI_ALLOCATOR_CALLBACK").unwrap() == "policy";
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("allocator controls"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    let mut allocated = service.attach(receiver).unwrap();
    let owner = Arc::new(OnceLock::<Weak<AllocatedReceiver>>::new());
    let entered = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let armed = Arc::new(AtomicBool::new(false));
    let changing_thread = Arc::new(Mutex::new(None));
    let on_change = Arc::new(AtomicU64::new(0));
    let allocations = Arc::new(AtomicU64::new(0));
    {
        let (owner, entered, released, finished, armed, changing_thread, on_change, allocations) = (
            Arc::clone(&owner),
            Arc::clone(&entered),
            Arc::clone(&released),
            Arc::clone(&finished),
            Arc::clone(&armed),
            Arc::clone(&changing_thread),
            Arc::clone(&on_change),
            Arc::clone(&allocations),
        );
        allocated
            .set_video_allocator(move |request| {
                if policy_case && !entered.swap(true, Ordering::AcqRel) {
                    while !released.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(1));
                    }
                    let owner = owner.get().unwrap().upgrade().unwrap();
                    controls(owner.receiver());
                    finished.store(true, Ordering::Release);
                }
                let buffer = AllocationBuffer::from_external(
                    request,
                    External {
                        memory: (0..request.minimum_bytes().div_ceil(64))
                            .map(|_| Block([0; 64]))
                            .collect(),
                        owner: Arc::clone(&owner),
                        armed: Arc::clone(&armed),
                        changing_thread: Arc::clone(&changing_thread),
                        on_change: Arc::clone(&on_change),
                    },
                )
                .unwrap();
                allocations.fetch_add(1, Ordering::Release);
                Ok(buffer)
            })
            .unwrap();
    }
    let allocated = Arc::new(allocated);
    owner.set(Arc::downgrade(&allocated)).unwrap();
    let stopped = AtomicBool::new(false);
    thread::scope(|scope| {
        let _stop = Stop(&stopped);
        scope.spawn(|| {
            let frame = VideoFrame::builder()
                .resolution(64, 64)
                .data(vec![1; 64 * 64 * 4])
                .build()
                .unwrap();
            while !stopped.load(Ordering::Acquire) {
                sender.send_video(&frame).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        let deadline = Instant::now() + PATIENCE;
        while if policy_case {
            !entered.load(Ordering::Acquire)
        } else {
            allocations.load(Ordering::Acquire) < 8 || service.usage().retained_allocations() == 0
        } {
            if policy_case {
                drop(allocated.receiver().capture_video_ref(Duration::ZERO));
            }
            assert!(
                Instant::now() < deadline,
                "allocator callback did not become ready"
            );
            thread::sleep(Duration::from_millis(1));
        }
        let (started, waiting) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let allocated = &allocated;
        let changing_thread = &changing_thread;
        let armed = &armed;
        scope.spawn(move || {
            *changing_thread.lock().unwrap() = Some(thread::current().id());
            armed.store(true, Ordering::Release);
            started.send(()).unwrap();
            match operation.as_str() {
                "connect_to" => allocated.receiver().connect_to(&source),
                "disconnect" => allocated.receiver().disconnect(),
                "reconnect" => allocated.receiver().reconnect(),
                _ => unreachable!(),
            }
            armed.store(false, Ordering::Release);
            done.send(()).unwrap();
        });
        waiting.recv_timeout(PATIENCE).unwrap();
        thread::sleep(Duration::from_millis(100));
        released.store(true, Ordering::Release);
        completed
            .recv_timeout(PATIENCE)
            .expect("connection change waited for allocator controls");
        if policy_case {
            assert!(
                finished.load(Ordering::Acquire),
                "policy did not finish its controls"
            );
        } else {
            assert!(
                on_change.load(Ordering::Acquire) > 0,
                "no external allocation was released on the changing thread"
            );
        }
        stopped.store(true, Ordering::Release);
    });
}
