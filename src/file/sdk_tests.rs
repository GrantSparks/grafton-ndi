//! These tests require the explicit isolated-network opt-in and a real File SDK.
use super::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn api() -> FileApi {
    assert_eq!(std::env::var("GRAFTON_NDI_ISOLATED_NETWORK").as_deref(), Ok("1"),
        "run File integration tests in an isolated network namespace and set GRAFTON_NDI_ISOLATED_NETWORK=1");
    let ndi = crate::NDI::new().expect("real selected runtime");
    FileApi::open(&ndi, &FileRuntimeOptions::default()).expect("matching File runtime")
}

#[cfg(all(unix, not(ndi_link_static)))]
#[test]
#[ignore = "requires an explicit genuine Standard runtime; run in its own isolated process"]
fn native_file_rejects_standard_posix_before_load() {
    let path = PathBuf::from(
        std::env::var_os("GRAFTON_NDI_TEST_STANDARD_RUNTIME")
            .expect("absolute Standard core library path"),
    );
    let ndi = crate::NDI::with_options(
        &crate::RuntimeOptions::default()
            .library_path(path)
            .edition(crate::SdkEdition::Standard),
    )
    .expect("Standard core runtime");
    // A deliberately nonexistent File path proves that edition gating precedes
    // any attempt to load a companion module or resolve its dependencies.
    let options = FileRuntimeOptions::default()
        .with_library_path(std::env::temp_dir().join("no-such-ndi-file-module"))
        .unwrap();
    assert!(matches!(
        FileApi::open(&ndi, &options),
        Err(FileError::Core(crate::Error::UnsupportedCapability { reason, .. }))
            if reason.starts_with("RuntimeEditionMismatch:")
    ));
}

#[cfg(all(target_os = "windows", target_arch = "x86", not(ndi_link_static)))]
#[test]
#[ignore = "requires a genuine Windows x86 core runtime"]
fn native_file_rejects_windows_x86_before_load() {
    let ndi = crate::NDI::new().expect("Windows x86 core runtime");
    let options = FileRuntimeOptions::default()
        .with_library_path(std::env::temp_dir().join("no-such-ndi-file-module.dll"))
        .unwrap();
    assert!(matches!(
        FileApi::open(&ndi, &options),
        Err(FileError::Core(crate::Error::UnsupportedCapability { reason, .. }))
            if reason.starts_with("TargetArtifactUnavailable:")
    ));
}

#[test]
#[ignore = "requires a genuine File SDK, isolated network and GRAFTON_NDI_FILE_FIXTURE"]
fn native_file_probe_and_player_controls() {
    let api = api();
    let fixture =
        PathBuf::from(std::env::var_os("GRAFTON_NDI_FILE_FIXTURE").expect("fixture path"));
    let info = api.probe(&fixture).expect("probe valid MKV");
    assert!(info.video_track_count() > 0);
    assert!(api.probe(fixture.with_extension("does-not-exist")).is_err());
    let mut player = api
        .create_player(PlayerOptions::new("isolated-file-player-test").unwrap())
        .unwrap();
    assert!(!player.is_running());
    player.set_paused(true);
    assert!(!player.is_paused(), "pause without a file has no effect");
    player.set_looping(true);
    let source = player.advertised_source().unwrap();
    player.start(&fixture).expect("start playback");
    assert!(player.is_looping());
    player.set_paused(true);
    assert!(player.is_paused());
    assert!(player.source_format().unwrap().is_some());
    player.seek(FilePosition::from_ticks(0)).expect("seek");
    player.set_paused(false);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut opened = false;
    while Instant::now() < deadline {
        if let Some(event) = player.poll_event(Duration::from_millis(50)).unwrap() {
            assert_ne!(
                event.kind(),
                PlayerEventKind::Error,
                "{}",
                event.message().to_string_lossy()
            );
            if event.kind() == PlayerEventKind::FileOpened {
                opened = true;
                break;
            }
        }
    }
    assert!(opened, "expected file-opened event");
    player.stop();
    assert!(!player.is_running());
    assert!(!player.is_paused());
    assert!(player.is_looping(), "loop is instance state");
    assert_eq!(
        player.advertised_source().unwrap(),
        source,
        "stop retains advertised identity"
    );
}

#[test]
#[ignore = "requires a genuine File SDK, isolated network and GRAFTON_NDI_FILE_FIXTURE"]
fn native_file_async_player_owner_thread() {
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    fn wait<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut future = std::pin::pin!(future);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "owner thread did not complete operation"
            );
            std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }
    let api = api();
    let fixture =
        PathBuf::from(std::env::var_os("GRAFTON_NDI_FILE_FIXTURE").expect("fixture path"));
    assert!(
        wait(AsyncFilePlayer::probe(api.clone(), fixture.clone()))
            .unwrap()
            .video_track_count()
            > 0
    );
    let player = wait(AsyncFilePlayer::create(
        api,
        PlayerOptions::new("isolated-async-file-player-test").unwrap(),
    ))
    .unwrap();
    wait(player.set_looping(true)).unwrap();
    wait(player.start(fixture)).unwrap();
    assert!(wait(player.is_running()).unwrap());
    assert!(wait(player.is_looping()).unwrap());
    wait(player.set_paused(true)).unwrap();
    assert!(wait(player.is_paused()).unwrap());
    assert!(wait(player.source_format()).unwrap().is_some());
    let retained = player.clone();
    wait(player.close()).expect("owner destroyed before close completes");
    assert!(
        wait(retained.is_running()).is_err(),
        "close invalidates all handles"
    );
}

#[test]
#[ignore = "requires a genuine File SDK and isolated-network writable scratch directory"]
fn native_file_record_finalize_and_probe() {
    let api = api();
    let ndi = crate::NDI::new().unwrap();
    let sender = crate::Sender::new(
        &ndi,
        &crate::SenderOptions::builder("isolated-file-recorder-test").build(),
    )
    .unwrap();
    let mut source = sender.source().unwrap();
    if matches!(source.address, crate::SourceAddress::None) {
        let finder = crate::Finder::new(
            &ndi,
            &crate::FinderOptions::builder()
                .extra_ips("127.0.0.1")
                .build(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(found) = finder.current_sources().unwrap().into_iter().find(|found| {
                found.name == source.name && !matches!(found.address, crate::SourceAddress::None)
            }) {
                source = found;
                break;
            }
            finder.wait_for_sources(Duration::from_millis(50)).unwrap();
        }
    }
    eprintln!("isolated recorder source: {source:?}");
    let input = match source.address {
        crate::SourceAddress::Ip(address) | crate::SourceAddress::Url(address) => {
            // Keep the native port but use only the namespace's loopback interface.
            let port = address
                .rsplit(':')
                .next()
                .and_then(|port| port.parse::<u16>().ok())
                .expect("sender address port");
            SourceSpec::from_url(format!("127.0.0.1:{port}")).unwrap()
        }
        crate::SourceAddress::None => SourceSpec::new(&source.name).unwrap(),
    };
    let directory = PathBuf::from(
        std::env::var_os("GRAFTON_NDI_FILE_SCRATCH").expect("private writable scratch"),
    );
    let filename = directory.join(format!("file-roundtrip-{}.mkv", std::process::id()));
    assert!(!filename.exists(), "test output must be new");
    let mut recorder = api
        .create_recorder(
            RecorderOptions::new(input)
                .exact_filename()
                .with_proxy(false)
                .with_tracks(TrackSelection::VIDEO),
        )
        .unwrap();
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = running.clone();
    let producer = std::thread::spawn(move || {
        let frame = crate::VideoFrame::builder()
            .resolution(320, 180)
            .pixel_format(crate::PixelFormat::BGRA)
            .build()
            .unwrap();
        while flag.load(std::sync::atomic::Ordering::Acquire) {
            sender.send_video(&frame);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    // Always stop the producer even if a checked SDK result fails below.
    struct Producer {
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Producer {
        fn drop(&mut self) {
            self.flag.store(false, std::sync::atomic::Ordering::Release);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
    let _producer = Producer {
        flag: running,
        join: Some(producer),
    };
    recorder
        .start(
            RecordingTarget::new(&filename).unwrap(),
            RecordingTimecode::Preserve,
        )
        .expect("record start");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut first_frame = false;
    while Instant::now() < deadline {
        if let Some(event) = recorder.poll_event(Duration::from_millis(50)).unwrap() {
            assert!(
                !matches!(
                    event.kind(),
                    RecorderEventKind::Error | RecorderEventKind::OpenFailed
                ),
                "{}",
                event.message().to_string_lossy()
            );
            if event.kind() == RecorderEventKind::FirstFrame {
                first_frame = true;
                break;
            }
        }
    }
    assert!(first_frame, "recorder did not capture loopback source");
    for _ in 0..10 {
        let _ = recorder.poll_event(Duration::from_millis(30)).unwrap();
    }
    let report = recorder.finish(Duration::from_secs(5)).unwrap();
    let FinishStatus::Closed(report) = report else {
        panic!("recording closure not confirmed: {report:?}")
    };
    assert!(report.closure_confirmed());
    assert_eq!(report.error_events(), 0);
    let file = api
        .probe(&filename)
        .expect("completed recording probes successfully");
    assert_eq!(file.video_track_count(), 1);
    assert!(file.duration().value().ticks() > 0);
    std::fs::remove_file(&filename).unwrap();
}
