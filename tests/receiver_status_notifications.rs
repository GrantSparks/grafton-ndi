mod common;

use grafton_ndi::{Error, MetadataFrame, Receiver, ReceiverOptions, Sender, SenderOptions, NDI};
use std::time::{Duration, Instant};

fn wait_until(stage: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "{stage} timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "Requires NDI runtime and loopback networking"]
fn media_capture_retains_capability_notifications() -> Result<(), Error> {
    let ndi = NDI::new()?;
    eprintln!("SDK: {} on {}", NDI::version()?, std::env::consts::OS);
    for kind in 0..6 {
        let sender = Sender::new(
            &ndi,
            &SenderOptions::builder(format!("status retention {kind}")).build(),
        )?;
        let source = common::loopback_source(&ndi, &sender)?;
        eprintln!("capture case {kind}: connecting to {source:?}");
        let options = ReceiverOptions::builder(source).build();
        let control = Receiver::new(&ndi, &options)?;
        let media = Receiver::new(&ndi, &options)?;
        wait_until("initial receiver connections", || {
            control.is_connected() && media.is_connected()
        });
        for receiver in [&control, &media] {
            while receiver
                .poll_status_change(Duration::from_millis(50))?
                .is_some()
            {}
            assert!(!receiver.ptz_is_supported());
        }
        // Exercise two distinct capability invalidations using the SDK's
        // Send_VirtualPTZ / Send_Capabilities XML conventions. Capability flag
        // reset behavior on idle receivers is not part of notification retention.
        for (update, capabilities) in [
            "<ndi_capabilities ntk_ptz=\"true\"/>",
            "<ndi_capabilities ntk_ptz=\"true\" web_control=\"http://127.0.0.1/status-control\"/>",
        ]
        .into_iter()
        .enumerate()
        {
            eprintln!("capture case {kind}: capability update {update}");
            sender.clear_connection_metadata();
            sender.add_connection_metadata(&MetadataFrame::with_data(capabilities, 0)?);
            if update == 0 {
                wait_until("PTZ capability enable", || {
                    control.ptz_is_supported() && media.ptz_is_supported()
                });
            }
            wait_until("control status notification", || {
                control
                    .poll_status_change(Duration::from_millis(10))
                    .unwrap()
                    .is_some()
            });
            // PTZ enable has a public readiness probe above. The independent
            // web-control change has no public getter here, so allow its SDK
            // notification to reach the media receiver during capture. Recv.h permits
            // web-control publication to take a second or two after connection.
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
                0 => assert!(media.video().try_capture(poll_timeout)?.is_none()),
                1 => assert!(media.video().try_capture_ref(poll_timeout)?.is_none()),
                2 => assert!(media.audio().try_capture(poll_timeout)?.is_none()),
                3 => assert!(media.metadata().try_capture(poll_timeout)?.is_none()),
                4 => assert!(matches!(
                    media.video().capture(retry_timeout),
                    Err(Error::FrameTimeout { .. })
                )),
                _ => std::thread::scope(|scope| {
                    let workers: Vec<_> = (0..3)
                        .map(|_| {
                            scope.spawn(|| {
                                assert!(media.video().try_capture(poll_timeout).unwrap().is_none());
                            })
                        })
                        .collect();
                    for worker in workers {
                        worker.join().unwrap();
                    }
                }),
            }
            assert!(
                media.poll_status_change(Duration::ZERO)?.is_some(),
                "capture kind {kind} lost status"
            );
            assert!(
                media.poll_status_change(Duration::ZERO)?.is_none(),
                "notification replayed"
            );
        }
    }
    Ok(())
}
