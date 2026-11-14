//! Connection targets and native requests stay in agreement, without an SDK.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{Error, Ndi, Receiver, ReceiverOptions, RuntimeOptions, Source};
use std::{fs, process::Command, sync::Barrier};

#[test]
fn connection_target_tracks_native_requests() {
    if std::env::var_os("GRAFTON_TARGET_CHILD").is_some() {
        exercise_target();
        return;
    }
    let directory = fixture::FixtureDir::create("connection-target");
    let library = directory.path().join("libndi_target.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects"],
        ..Default::default()
    }
    .build(&library);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection_target_tracks_native_requests",
            "--nocapture",
        ])
        .env("GRAFTON_TARGET_CHILD", &library)
        .env(
            "GRAFTON_FAKE_NDI_LOG",
            directory.path().join("requests.log"),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn exercise_target() {
    let path = std::path::PathBuf::from(std::env::var_os("GRAFTON_TARGET_CHILD").unwrap());
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(path)).unwrap();
    let a = Source::new("A");
    let b = Source::new("B");
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(a.clone())).unwrap();
    let initial = receiver.connection_target();
    assert_eq!(initial, a);
    let _: () = receiver.reconnect();
    assert_eq!(receiver.connection_target(), a);
    receiver.connect_to(&b).unwrap();
    assert_eq!(receiver.connection_target(), b);
    assert_eq!(initial, a, "a snapshot does not change with the receiver");
    receiver.disconnect();
    assert_eq!(receiver.connection_target(), b);
    receiver.reconnect();
    assert_eq!(receiver.connection_target(), b);
    assert!(matches!(
        receiver.connect_to(&Source::new("bad\0name")),
        Err(Error::InvalidCString(_))
    ));
    assert_eq!(receiver.connection_target(), b);
    let log = std::path::PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap());
    let requests = || {
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("receiver-connect:").map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert_eq!(requests(), ["A", "B", "<disconnected>", "B"]);

    let start = Barrier::new(8);
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let receiver = &receiver;
            let start = &start;
            scope.spawn(move || {
                start.wait();
                for request in 0..25 {
                    receiver
                        .connect_to(&Source::new(format!("{worker}/{request}")))
                        .unwrap();
                    receiver.reconnect();
                }
            });
        }
    });
    let concurrent = requests();
    assert_eq!(concurrent.len(), 404);
    // Each explicit target is unique. A reconnect can repeat only the current
    // target; a target reappearing after another would expose a stale retry.
    let mut transitions = concurrent[4..].to_vec();
    transitions.dedup();
    assert_eq!(
        transitions
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        transitions.len()
    );
    assert_eq!(
        requests().last().unwrap(),
        receiver.connection_target().name()
    );
    // Retrying after the contention must send exactly the retained target.
    let target = receiver.connection_target();
    receiver.disconnect();
    receiver.reconnect();
    assert_eq!(requests().last().unwrap(), target.name());
}
