//! Transport interoperability between two independently loaded SDK versions.
//!
//! Set GRAFTON_NDI_PEER_RUNTIME, GRAFTON_NDI_PEER_VERSION and
//! GRAFTON_NDI_PEER_EDITION to require a peer from a second pinned SDK. The
//! child inherits the test runner's network confinement; both peers use only
//! loopback. Each process retains exactly one native implementation.
#![cfg(not(ndi_link_static))]

mod common;

use grafton_ndi::{
    capabilities::SdkEdition, AudioFrame, MetadataFrame, PixelFormat, Receiver,
    ReceiverColorFormat, ReceiverOptions, RuntimeOptions, Sender, SenderOptions, Source,
    SourceAddress, VideoFrame, NDI,
};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(20);
const TIMECODE: i64 = 123_456_789;
const METADATA: &str = "<grafton-cross-sdk/>";

struct Peer {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl Peer {
    fn finish(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "the peer failed: {status}");
                self.reader.take().unwrap().join().unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "the peer did not shut down");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Also reap the child and output reader when an assertion unwinds.
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn serve() {
    let path = std::env::var_os("GRAFTON_NDI_PEER_RUNTIME").unwrap();
    let ndi = NDI::with_options(
        &RuntimeOptions::default()
            .library_path(path)
            .edition(SdkEdition::Standard),
    )
    .unwrap();
    common::assert_expected_identity(ndi.info());
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder(common::unique_name("cross-sdk"))
            .clock_video(true)
            .clock_audio(false)
            .build(),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let SourceAddress::Ip(address) = source.address else {
        panic!("no loopback endpoint")
    };
    // Native diagnostics need not be UTF-8. Exercise the same pipe before our
    // UTF-8 control record, so they cannot break readiness on Windows.
    std::io::stdout()
        .write_all(b"\xffnative diagnostic\n")
        .unwrap();
    println!("GRAFTON_PEER_READY {address} {}", source.name);
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(30, 1)
        .data([40, 120, 200, 255].repeat(64 * 32))
        .timecode(TIMECODE)
        .metadata(METADATA)
        .build()
        .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .channels(2)
        .samples(1600)
        .data(vec![0.25; 3200])
        .timecode(TIMECODE)
        .build()
        .unwrap();
    let metadata = MetadataFrame::with_data(METADATA, TIMECODE).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        stopped.store(true, Ordering::Release);
    });
    let deadline = Instant::now() + TIMEOUT;
    while !stop.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "the parent never stopped the peer"
        );
        sender.send_video(&video);
        sender.send_audio(&audio);
        sender.send_metadata(&metadata);
    }
}

#[test]
fn different_sdk_versions_exchange_video_audio_and_metadata() {
    if std::env::var_os("GRAFTON_NDI_PEER_CHILD").is_some() {
        serve();
        return;
    }
    let Some(path) = std::env::var_os("GRAFTON_NDI_PEER_RUNTIME") else {
        let _ = writeln!(
            std::io::stderr(),
            "skipped: cross-SDK transport needs GRAFTON_NDI_PEER_RUNTIME"
        );
        return;
    };
    let version = std::env::var("GRAFTON_NDI_PEER_VERSION").expect("a pinned peer version");
    let edition = std::env::var("GRAFTON_NDI_PEER_EDITION").expect("a pinned peer edition");
    let ndi = common::ndi().expect("a configured peer test also requires the primary runtime");
    assert_ne!(
        ndi.info().version().unwrap().to_string(),
        version,
        "this test must exercise different runtime versions"
    );
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "different_sdk_versions_exchange_video_audio_and_metadata",
            "--nocapture",
        ])
        .env("GRAFTON_NDI_PEER_CHILD", "1")
        .env("GRAFTON_NDI_EXPECTED_RUNTIME", path)
        .env("GRAFTON_NDI_EXPECTED_VERSION", version)
        .env("GRAFTON_NDI_EXPECTED_EDITION", edition)
        .env("GRAFTON_NDI_REQUIRE_RUNTIME", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut peer = Peer {
        child,
        reader: None,
    };
    let stdout = peer.child.stdout.take().unwrap();
    let (ready, endpoint) = mpsc::channel();
    peer.reader = Some(thread::spawn(move || {
        let mut ready = Some(ready);
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).unwrap() != 0 {
            if let Some(source) = line.strip_prefix(b"GRAFTON_PEER_READY ") {
                let source = std::str::from_utf8(source)
                    .unwrap()
                    .trim_end_matches(['\r', '\n']);
                if let Some(ready) = ready.take() {
                    let _ = ready.send(source.to_owned());
                }
            }
            line.clear();
        }
    }));
    let source = endpoint
        .recv_timeout(TIMEOUT)
        .expect("the peer did not announce its endpoint");
    let (address, name) = source
        .split_once(' ')
        .expect("peer address and source name");
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::builder(Source {
            name: name.to_owned(),
            address: SourceAddress::Ip(address.to_owned()),
        })
        .color(ReceiverColorFormat::BGRX_BGRA)
        .build(),
    )
    .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let (mut video, mut audio, mut metadata) = (false, false, false);
    while !(video && audio && metadata) {
        assert!(
            Instant::now() < deadline,
            "cross-SDK delivery: video={video}, audio={audio}, metadata={metadata}"
        );
        if let Some(frame) = receiver
            .video()
            .try_capture(Duration::from_millis(20))
            .unwrap()
        {
            assert_eq!((frame.width(), frame.height()), (64, 32));
            assert!(matches!(
                frame.pixel_format(),
                PixelFormat::BGRA | PixelFormat::BGRX
            ));
            assert_eq!(frame.timecode(), TIMECODE);
            assert_eq!(frame.metadata(), Some(METADATA));
            let (pixels, remainder) = frame.data().as_chunks::<4>();
            assert!(remainder.is_empty());
            assert_eq!(pixels.len(), 64 * 32);
            assert!(pixels.iter().all(|pixel| pixel[..3]
                .iter()
                .zip([40u8, 120, 200])
                .all(|(&actual, expected)| actual.abs_diff(expected) <= 8)));
            video = true;
        }
        if let Some(frame) = receiver
            .audio()
            .try_capture(Duration::from_millis(20))
            .unwrap()
        {
            assert_eq!((frame.sample_rate(), frame.num_channels()), (48_000, 2));
            assert_eq!(frame.timecode(), TIMECODE);
            assert!(!frame.data().is_empty());
            assert!(frame
                .data()
                .iter()
                .all(|value| (*value - 0.25).abs() < 0.001));
            audio = true;
        }
        if let Some(frame) = receiver
            .metadata()
            .try_capture(Duration::from_millis(20))
            .unwrap()
        {
            if frame.data() == METADATA {
                assert_eq!(frame.timecode(), TIMECODE);
                metadata = true;
            }
        }
    }
    drop(receiver);
    peer.finish();
}
