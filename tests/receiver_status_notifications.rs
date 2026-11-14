#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    Error, MetadataFrame, Receiver, ReceiverEvent, ReceiverOptions, Sender, SenderOptions, Timecode,
};
use std::time::{Duration, Instant};

fn wait_until(stage: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "{stage} timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn media_capture_retains_capability_notifications() -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    eprintln!(
        "SDK: {} on {}",
        ndi.info().raw_version(),
        std::env::consts::OS
    );
    // Keep every source alive so each capture API uses a distinct live endpoint
    // rather than immediately reusing a retired sender's address. Rapid sender
    // teardown is covered by the separate lifecycle stress test.
    let senders = (0..6)
        .map(|kind| {
            Sender::new(
                &ndi,
                &SenderOptions::new(common::unique_name(&format!("status retention {kind}"))),
            )
        })
        .collect::<Result<Vec<_>, Error>>()?;
    for (kind, sender) in senders.iter().enumerate() {
        let source = common::loopback_source(&ndi, sender)?;
        eprintln!("capture case {kind}: connecting to {source:?}");
        // Each capture API must retain this receiver's own capability event.
        let options = ReceiverOptions::new(source);
        let media = Receiver::new(&ndi, &options)?;
        wait_until("initial receiver connection", || media.is_connected());
        while media.poll_event(Duration::from_millis(50))?.is_some() {}
        assert!(!media.ptz().is_supported());
        // Exercise two distinct capability invalidations using the SDK's
        // Send_VirtualPTZ / Send_Capabilities XML conventions. Capability flag
        // reset behavior on idle receivers is not part of notification retention.
        // Retain the existing single-invalidation coverage on Windows.
        // Repeated publication/consumption is also covered by deterministic
        // receiver unit tests on every platform.
        let updates = if cfg!(target_os = "windows") { 1 } else { 2 };
        for (update, capabilities) in [
            "<ndi_capabilities ntk_ptz=\"true\"/>",
            "<ndi_capabilities ntk_ptz=\"true\" web_control=\"http://127.0.0.1/status-control\"/>",
        ]
        .into_iter()
        .take(updates)
        .enumerate()
        {
            eprintln!("capture case {kind}: capability update {update}");
            sender.clear_connection_metadata();
            sender.add_connection_metadata(
                &MetadataFrame::new(capabilities)?.with_timecode(Timecode::from_ticks(0)),
            );
            if update == 0 {
                wait_until("PTZ capability enable", || media.ptz().is_supported());
            } else {
                wait_until("web-control capability update", || {
                    media.web_control_url().unwrap().as_deref()
                        == Some("http://127.0.0.1/status-control")
                });
            }
            // Both capability getters observe this receiver without consuming
            // its status notification. Keep the existing SDK capture budgets.
            let poll_timeout = if update == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs(3)
            };
            let retry_timeout = if update == 0 {
                Duration::from_millis(20)
            } else {
                Duration::from_secs(3)
            };
            match kind {
                0 => assert!(media.video().capture(poll_timeout)?.is_none()),
                1 => assert!(media.video().capture_ref(poll_timeout)?.is_none()),
                2 => assert!(media.audio().capture(poll_timeout)?.is_none()),
                3 => {
                    // SDK 6.3 can also deliver capability XML as ordinary
                    // metadata. Consume those valid frames until the status/no-
                    // frame result, rather than assuming a frameless stream.
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while media.metadata().capture(poll_timeout)?.is_some() {
                        assert!(Instant::now() < deadline, "metadata queue did not drain");
                    }
                }
                4 => assert!(media.video().capture(retry_timeout)?.is_none()),
                _ => std::thread::scope(|scope| {
                    let workers: Vec<_> = (0..3)
                        .map(|_| {
                            scope.spawn(|| {
                                assert!(media.video().capture(poll_timeout).unwrap().is_none());
                            })
                        })
                        .collect();
                    for worker in workers {
                        worker.join().unwrap();
                    }
                }),
            }
            assert!(
                matches!(
                    media.poll_event(Duration::ZERO)?,
                    Some(ReceiverEvent::StatusChanged(_))
                ),
                "capture kind {kind} lost status"
            );
            assert!(
                !matches!(
                    media.poll_event(Duration::ZERO)?,
                    Some(ReceiverEvent::StatusChanged(_))
                ),
                "notification replayed"
            );
        }
    }
    Ok(())
}
