//! An async receiver reaches its whole receiver: the status changes that its
//! captures pass over are returned by `AsyncReceiver::poll_event`, as
//! `receiver_status_notifications.rs` shows for the synchronous captures, and
//! its async connection changes reach the SDK receiver.
#![cfg(any(feature = "tokio", feature = "smol"))]

mod common;

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    Error, MetadataFrame, Ndi, Receiver, ReceiverEvent, ReceiverOptions, RuntimeFeature, Sender,
    SenderOptions, Source, SourceNameChange, Timecode,
};
use std::time::{Duration, Instant};

fn wait_until(stage: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "{stage} timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Each async capture kind keeps the status change its receiver's capability
/// update causes, for `AsyncReceiver::poll_event`. The waits for the SDK to
/// see each update use the lent receiver's controls.
async fn async_capture_retains_capability_notifications<R: AsyncRuntime>(
    ndi: &Ndi,
    runtime: &str,
) -> Result<(), Error> {
    // Keep every source alive so each capture kind uses a distinct live
    // endpoint rather than immediately reusing a retired sender's address.
    let senders = (0..3)
        .map(|kind| {
            Sender::new(
                ndi,
                &SenderOptions::new(common::unique_name(&format!(
                    "async status retention {runtime} {kind}"
                ))),
            )
        })
        .collect::<Result<Vec<_>, Error>>()?;
    for (kind, sender) in senders.iter().enumerate() {
        let source = common::loopback_source(ndi, sender)?;
        eprintln!("{runtime} capture case {kind}: connecting to {source:?}");
        let media = AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(source))?);
        wait_until("initial receiver connection", || media.is_connected());
        while media.poll_event(Duration::from_millis(50)).await?.is_some() {}
        assert!(!media.receiver().ptz().is_supported());
        // The same two capability invalidations as the synchronous test, with
        // its single-invalidation coverage on Windows.
        let updates = if cfg!(target_os = "windows") { 1 } else { 2 };
        for (update, capabilities) in [
            "<ndi_capabilities ntk_ptz=\"true\"/>",
            "<ndi_capabilities ntk_ptz=\"true\" web_control=\"http://127.0.0.1/status-control\"/>",
        ]
        .into_iter()
        .take(updates)
        .enumerate()
        {
            eprintln!("{runtime} capture case {kind}: capability update {update}");
            sender.clear_connection_metadata();
            sender.add_connection_metadata(
                &MetadataFrame::new(capabilities)?.with_timecode(Timecode::from_ticks(0)),
            );
            if update == 0 {
                wait_until("PTZ capability enable", || {
                    media.receiver().ptz().is_supported()
                });
            } else {
                wait_until("web-control capability update", || {
                    media.receiver().web_control_url().unwrap().as_deref()
                        == Some("http://127.0.0.1/status-control")
                });
            }
            // A zero budget makes one attempt; three seconds wait across the
            // status change, so the capture passes over it.
            let timeout = if update == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs(3)
            };
            match kind {
                0 => assert!(media.video().capture(timeout).await?.is_none()),
                1 => assert!(media.audio().capture(timeout).await?.is_none()),
                _ => {
                    // The SDK can also deliver capability XML as ordinary
                    // metadata. Consume those frames until the status result.
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while media.metadata().capture(timeout).await?.is_some() {
                        assert!(Instant::now() < deadline, "metadata queue did not drain");
                    }
                }
            }
            assert!(
                matches!(
                    media.poll_event(Duration::ZERO).await?,
                    Some(ReceiverEvent::StatusChanged(_))
                ),
                "{runtime} capture kind {kind} lost status"
            );
            assert!(
                !matches!(
                    media.poll_event(Duration::ZERO).await?,
                    Some(ReceiverEvent::StatusChanged(_))
                ),
                "notification replayed"
            );
        }
    }
    Ok(())
}

/// `connect_to`, `disconnect` and `reconnect` reach the receiver and leave its
/// original target alone; the new connection's name is reported where the
/// runtime can report it.
async fn async_connection_changes_reach_the_receiver<R: AsyncRuntime>(
    ndi: &Ndi,
    runtime: &str,
) -> Result<(), Error> {
    let sender = Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(&format!("async connect {runtime}"))),
    )?;
    let source = common::loopback_source(ndi, &sender)?;
    // Created for no source at all, so only `connect_to` can connect it.
    let nowhere = Source::new("");
    let media =
        AsyncReceiver::<R>::new(Receiver::new(ndi, &ReceiverOptions::new(nowhere.clone()))?);
    assert!(!media.is_connected());

    media.connect_to(&source).await?;
    wait_until("connect_to", || media.is_connected());
    if ndi
        .require_feature(RuntimeFeature::ConnectedSourceName)
        .is_ok()
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "no source-name change reported");
            match media
                .poll_source_name_change(Duration::from_millis(500))
                .await?
            {
                Some(SourceNameChange::Connected(name)) if name == source.name() => {
                    eprintln!("{runtime}: source-name change to {name:?}");
                    break;
                }
                change => eprintln!("{runtime}: source-name change {change:?}"),
            }
        }
    }

    media.disconnect().await?;
    wait_until("disconnect", || !media.is_connected());
    media.connect_to(&source).await?;
    wait_until("connect_to after disconnect", || media.is_connected());
    // `reconnect` returns to the original target, which is no source.
    media.reconnect().await?;
    wait_until("reconnect to the original target", || !media.is_connected());
    assert_eq!(media.receiver().source(), &nowhere);
    Ok(())
}

#[cfg(feature = "tokio")]
mod tokio_runtime {
    use super::*;
    use grafton_ndi::asynchronous::Tokio;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(future)
    }

    #[test]
    fn tokio_capture_retains_notifications_for_poll_event() -> Result<(), Error> {
        let Some(ndi) = common::ndi() else {
            return Ok(());
        };
        block_on(async_capture_retains_capability_notifications::<Tokio>(
            &ndi, "tokio",
        ))
    }

    #[test]
    fn tokio_connection_changes_reach_the_receiver() -> Result<(), Error> {
        let Some(ndi) = common::ndi() else {
            return Ok(());
        };
        block_on(async_connection_changes_reach_the_receiver::<Tokio>(
            &ndi, "tokio",
        ))
    }
}

#[cfg(feature = "smol")]
mod smol_runtime {
    use super::*;
    use grafton_ndi::asynchronous::Smol;

    #[test]
    fn smol_capture_retains_notifications_for_poll_event() -> Result<(), Error> {
        let Some(ndi) = common::ndi() else {
            return Ok(());
        };
        smol::block_on(async_capture_retains_capability_notifications::<Smol>(
            &ndi, "smol",
        ))
    }

    #[test]
    fn smol_connection_changes_reach_the_receiver() -> Result<(), Error> {
        let Some(ndi) = common::ndi() else {
            return Ok(());
        };
        smol::block_on(async_connection_changes_reach_the_receiver::<Smol>(
            &ndi, "smol",
        ))
    }
}
