mod common;

use grafton_ndi::{Error, MetadataFrame, Receiver, ReceiverOptions, Sender, SenderOptions, NDI};
use std::time::{Duration, Instant};

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "loopback capability update timed out"
        );
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
        let options = ReceiverOptions::builder(source).build();
        let control = Receiver::new(&ndi, &options)?;
        let media = Receiver::new(&ndi, &options)?;
        wait_until(|| control.is_connected() && media.is_connected());
        for receiver in [&control, &media] {
            while receiver
                .poll_status_change(Duration::from_millis(50))?
                .is_some()
            {}
            assert!(!receiver.ptz_is_supported());
        }
        for supported in [true, false] {
            sender.clear_connection_metadata();
            sender.add_connection_metadata(&MetadataFrame::with_data(
                format!("<ndi_capabilities ntk_ptz=\"{supported}\"/>"),
                0,
            )?);
            wait_until(|| {
                control.ptz_is_supported() == supported && media.ptz_is_supported() == supported
            });
            assert!(control
                .poll_status_change(Duration::from_millis(100))?
                .is_some());
            match kind {
                0 => assert!(media.video().try_capture(Duration::ZERO)?.is_none()),
                1 => assert!(media.video().try_capture_ref(Duration::ZERO)?.is_none()),
                2 => assert!(media.audio().try_capture(Duration::ZERO)?.is_none()),
                3 => assert!(media.metadata().try_capture(Duration::ZERO)?.is_none()),
                4 => assert!(matches!(
                    media.video().capture(Duration::from_millis(20)),
                    Err(Error::FrameTimeout { .. })
                )),
                _ => std::thread::scope(|scope| {
                    let workers: Vec<_> = (0..3)
                        .map(|_| {
                            scope.spawn(|| {
                                assert!(media
                                    .video()
                                    .try_capture(Duration::ZERO)
                                    .unwrap()
                                    .is_none());
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
