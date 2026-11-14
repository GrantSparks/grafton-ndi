//! Native source-name cancellation with direct proof the abandoned worker ended.
use super::*;
use crate::{
    Finder, FinderOptions, ReceiverOptions, Sender, SenderOptions, SourceAddress, VideoFrame,
};
use std::{
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::Instant,
};

fn loopback_source(ndi: &crate::Ndi, sender: &Sender) -> Source {
    let name = sender.source().unwrap().name().to_owned();
    let finder = Finder::new(ndi, &FinderOptions::new().extra_ips("127.0.0.1")).unwrap();
    let source = finder
        .find_source(Duration::from_secs(15), |source| {
            source.name() == name && matches!(source.address(), Some(SourceAddress::Ip(_)))
        })
        .unwrap()
        .expect("the local sender must be discovered");
    let port = source
        .address()
        .unwrap()
        .port()
        .expect("an advertised port");
    source.with_address(SourceAddress::Ip(format!("127.0.0.1:{port}")))
}

struct Publishing {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Publishing {
    fn start(sender: Sender) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let sending = Arc::clone(&running);
        let thread = thread::spawn(move || {
            let video = VideoFrame::builder().resolution(64, 32).build().unwrap();
            while sending.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            running,
            thread: Some(thread),
        }
    }
}

impl Drop for Publishing {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn cancelled_query_releases_before_connecting<R: AsyncRuntime>(runtime: &str) {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    if ndi
        .require_feature(crate::RuntimeFeature::ConnectedSourceName)
        .is_err()
    {
        eprintln!("skipped: this runtime has no connected-source-name query");
        return;
    }
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(crate::test_support::unique_name(&format!(
            "cancel source {runtime}"
        )))
        .clock_video(false)
        .clock_audio(false),
    )
    .unwrap();
    let source = loopback_source(&ndi, &sender);
    let _publishing = Publishing::start(sender);
    let receiver = AsyncReceiver::<R>::new(
        Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap(),
    );
    while receiver
        .poll_source_name_change(Duration::ZERO)
        .await
        .unwrap()
        .is_some()
    {}

    let mut abandoned = Box::pin(receiver.poll_source_name_change(Duration::from_secs(4)));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        abandoned.as_mut().poll(&mut context),
        Poll::Pending
    ));
    // Only this handle and its submitted work own the receiver. Native entry
    // timing is covered by the scripted boundary; give this worker a chance
    // to start without treating that pause as evidence of its completion.
    thread::sleep(Duration::from_millis(30));
    drop(abandoned);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Arc::strong_count(&receiver.inner) != 1 {
        assert!(
            Instant::now() < deadline,
            "{runtime} retained its abandoned source-name worker"
        );
        thread::sleep(Duration::from_millis(1));
    }
    // A sole Arc proves the old worker fully released this SAME receiver.
    // No connection or source-name change was requested before this point.
    receiver.receiver().connect_to(&source).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if receiver
            .receiver()
            .video()
            .capture_ref(Duration::from_millis(100))
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{runtime} received no media from its new source"
        );
    }
    // Video delivery establishes native connection readiness without consuming
    // the separate source-name query's one-shot change.
    assert_eq!(
        receiver
            .poll_source_name_change(Duration::from_secs(3))
            .await
            .unwrap(),
        Some(SourceNameChange::Connected(source.name().to_owned())),
        "{runtime} lost the change after its abandoned worker fully ended",
    );
}

#[cfg(feature = "tokio")]
#[test]
fn abandoned_tokio_source_name_wait_releases_before_later_native_changes() {
    ::tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(cancelled_query_releases_before_connecting::<Tokio>("tokio"));
}

#[cfg(feature = "smol")]
#[test]
fn abandoned_smol_source_name_wait_releases_before_later_native_changes() {
    ::smol::block_on(cancelled_query_releases_before_connecting::<Smol>("smol"));
}
