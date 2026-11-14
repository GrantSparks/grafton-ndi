//! Dropped waits stop after their in-flight native attempt, on both runtimes.
#![cfg(all(
    target_os = "linux",
    not(ndi_link_static),
    any(feature = "tokio", feature = "smol")
))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    Ndi, Receiver, ReceiverOptions, RuntimeOptions, Source,
};
use std::{
    fs,
    future::Future,
    process::Command,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

fn log() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap_or_default()
}

fn calls(source_name: bool) -> usize {
    let prefix = if source_name {
        "source-name:"
    } else {
        "capture:"
    };
    log()
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

fn destroyed() -> usize {
    log()
        .lines()
        .filter(|&line| line == "receiver-destroy")
        .count()
}

async fn cancelled_waits<R: AsyncRuntime>(ndi: &Ndi) {
    for kind in 0..4 {
        let receiver = AsyncReceiver::<R>::new(
            Receiver::new(ndi, &ReceiverOptions::new(Source::new("cancel fixture"))).unwrap(),
        );
        let before = calls(kind == 3);
        let before_destroyed = destroyed();
        let mut future = Box::pin(async {
            let budget = Duration::from_secs(30);
            match kind {
                0 => {
                    receiver.video().capture(budget).await.unwrap();
                }
                1 => {
                    receiver.capture_any(budget).await.unwrap();
                }
                2 => {
                    receiver.poll_event(budget).await.unwrap();
                }
                _ => {
                    receiver.poll_source_name_change(budget).await.unwrap();
                }
            }
        });
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        let deadline = Instant::now() + Duration::from_secs(3);
        while calls(kind == 3) == before {
            assert!(
                Instant::now() < deadline,
                "wait {kind} never entered the SDK"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(future);
        drop(receiver);
        while destroyed() == before_destroyed {
            assert!(
                Instant::now() < deadline,
                "wait {kind} retained its receiver"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            calls(kind == 3),
            before + 1,
            "wait {kind} attempted after cancellation"
        );
        assert_eq!(destroyed(), before_destroyed + 1);
    }
    for line in log()
        .lines()
        .filter(|line| line.starts_with("source-name:"))
    {
        assert!(
            line.strip_prefix("source-name:")
                .unwrap()
                .parse::<u32>()
                .unwrap()
                <= 100
        );
    }
}

#[test]
fn abandoned_waits_release_the_receiver_without_spending_their_timeout() {
    if let Some(library) = std::env::var_os("GRAFTON_CANCEL_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        #[cfg(feature = "tokio")]
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(cancelled_waits::<grafton_ndi::asynchronous::Tokio>(&ndi));
        #[cfg(feature = "smol")]
        ::smol::block_on(cancelled_waits::<grafton_ndi::asynchronous::Smol>(&ndi));
        return;
    }
    let directory = fixture::FixtureDir::create("async-cancellation");
    let library = directory.path().join("libndi_cancel.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture", "fake_source_name"],
        ..Default::default()
    }
    .build(&library);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abandoned_waits_release_the_receiver_without_spending_their_timeout",
            "--nocapture",
        ])
        .env("GRAFTON_CANCEL_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("cancel.log"))
        .env("GRAFTON_FAKE_CAPTURE_DELAY_MS", "80")
        .env("GRAFTON_FAKE_SOURCE_NAME_DELAY_MS", "80")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
