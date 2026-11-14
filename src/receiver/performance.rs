//! Ignored native measurement of the retry policy, with real queued media.

use super::*;
use crate::{Finder, FinderOptions, Sender, SenderOptions, SourceAddress};

#[test]
#[ignore = "manual native notification-to-frame delay comparison"]
fn native_notification_capture_resources() -> Result<()> {
    let Some(ndi) = crate::test_support::ndi() else {
        return Ok(());
    };
    eprintln!(
        "runtime,{} {:?}",
        ndi.info().raw_version(),
        ndi.info().edition()
    );
    let name = crate::test_support::unique_name("notification latency");
    let sender = Sender::new(&ndi, &SenderOptions::new(&name).clock_video(false))?;
    let finder = Finder::new(&ndi, &FinderOptions::new().extra_ips("127.0.0.1"))?;
    let advertised = sender.source()?.name().to_owned();
    let source = finder
        .find_source_by_name(&advertised, Duration::from_secs(10))?
        .expect("sender discovered");
    let address = match source.address().expect("discovered endpoint") {
        SourceAddress::Ip(address) | SourceAddress::Url(address) => address,
    };
    let port: u16 = address
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .expect("advertised port");
    let source = source.with_address(SourceAddress::Ip(format!("127.0.0.1:{port}")));
    let stop = AtomicBool::new(false);
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    std::thread::scope(|scope| -> Result<()> {
        scope.spawn(|| {
            let frame = VideoFrame::builder().resolution(640, 360).build().unwrap();
            while !stop.load(Ordering::Acquire) {
                sender.send_video(&frame).unwrap();
                std::thread::sleep(Duration::from_millis(8));
            }
        });
        let _stop = Stop(&stop);
        // Alternate policy order. Fresh receivers leave their connection
        // notifications unconsumed while the producer queues real video.
        for round in 0..23 {
            for index in 0..2 {
                let previous = (round + index) % 2 == 0;
                let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone()))?;
                let connect_deadline = Instant::now() + Duration::from_secs(10);
                while !receiver.is_connected() {
                    assert!(
                        Instant::now() < connect_deadline,
                        "receiver connection timed out"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Connection queries do not consume native notifications.
                // Queue readiness is deliberately outside the timing boundary.
                std::thread::sleep(Duration::from_millis(50));
                let mut first_notification = None;
                let mut notifications = 0;
                let mut idle = 0;
                let mut pauses = 0;
                let mut requested_pause = Duration::ZERO;
                let start = Instant::now();
                let frame = capture_within_clock(
                    Deadline::after(Duration::from_secs(1))?,
                    |attempt| {
                        let outcome = receiver.core.poll_ref_until::<VideoKind>(attempt)?;
                        match outcome {
                            Retry::Progress => {
                                first_notification.get_or_insert_with(Instant::now);
                                notifications += 1;
                                // Before e7ced22 every notification was an
                                // empty result. Map that policy explicitly,
                                // preserving the same SDK, locks and deadline.
                                Ok(if previous {
                                    Retry::Idle
                                } else {
                                    Retry::Progress
                                })
                            }
                            Retry::Idle => {
                                idle += 1;
                                Ok(Retry::Idle)
                            }
                            Retry::Item(frame) => Ok(Retry::Item(frame)),
                        }
                    },
                    Instant::now,
                    |pause| {
                        pauses += 1;
                        requested_pause += pause;
                        std::thread::sleep(pause);
                    },
                )?
                .expect("queued video delivered");
                let end = Instant::now();
                assert_eq!((frame.width(), frame.height()), (640, 360));
                drop(frame);
                let Some(notification) = first_notification else {
                    panic!("sample observed no native notification; cannot qualify its latency");
                };
                assert!(
                    receiver.core.take_pending_event().is_some(),
                    "notification retained"
                );
                if round >= 3 {
                    eprintln!("notification,{previous},{},{notifications},{idle},{pauses},{:.6},{:.6},{:.6}", round - 3,
                        requested_pause.as_secs_f64() * 1000.0,
                        end.duration_since(start).as_secs_f64() * 1000.0,
                        end.duration_since(notification).as_secs_f64() * 1000.0);
                }
            }
        }
        Ok(())
    })
}
