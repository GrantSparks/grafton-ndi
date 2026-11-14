//! Manual public-adapter baseline. No streaming API or worker prototype ships
//! from these measurements. Run with the isolated runner and an NDI runtime.
#![cfg(any(feature = "tokio", feature = "smol"))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    AudioFrame, Receiver, ReceiverBandwidth, ReceiverOptions, Result, Sender, SenderOptions,
    Timecode,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

async fn sample<R: AsyncRuntime>(
    receiver: Receiver,
    epoch: Instant,
    callback: Duration,
) -> Result<Vec<f64>> {
    let receiver = AsyncReceiver::<R>::new(receiver);
    let mut samples = Vec::with_capacity(100);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut delivered = 0;
    while delivered < 120 {
        assert!(Instant::now() < deadline, "audio benchmark stalled");
        if let Some(sent) = receiver
            .audio()
            .capture_with(Duration::from_millis(100), move |frame| {
                let start = Instant::now();
                while start.elapsed() < callback {
                    std::hint::spin_loop();
                }
                frame.timecode().ticks()
            })
            .await?
        {
            if delivered >= 20 {
                samples.push(epoch.elapsed().as_secs_f64() * 1e6 - sent as f64 / 10.0);
            }
            delivered += 1;
        }
    }
    Ok(samples)
}

#[test]
#[ignore = "manual public async audio latency baseline requiring a native NDI runtime"]
fn async_capture_resources() -> Result<()> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    eprintln!(
        "runtime,{} {:?}",
        ndi.info().raw_version(),
        ndi.info().edition()
    );
    #[cfg(feature = "tokio")]
    let tokio = ::tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    for lanes in [1, 8] {
        for block in [48, 480] {
            for callback_us in [0, 50] {
                for mode in ["tokio", "smol"] {
                    if (mode == "tokio" && !cfg!(feature = "tokio"))
                        || (mode == "smol" && !cfg!(feature = "smol"))
                    {
                        continue;
                    }
                    let epoch = Instant::now();
                    let stop = AtomicBool::new(false);
                    // Stop even when a consumer returns an error or unwinds.
                    struct Stop<'a>(&'a AtomicBool);
                    impl Drop for Stop<'_> {
                        fn drop(&mut self) {
                            self.0.store(true, Ordering::Release);
                        }
                    }
                    let mut pairs = Vec::new();
                    for lane in 0..lanes {
                        let sender = Sender::new(
                            &ndi,
                            &SenderOptions::new(common::unique_name(&format!(
                                "dispatch {mode} {lane}"
                            )))
                            .clock_audio(false),
                        )?;
                        let source = common::loopback_source(&ndi, &sender)?;
                        let receiver = Receiver::new(
                            &ndi,
                            &ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::AudioOnly),
                        )?;
                        pairs.push((sender, receiver));
                    }
                    thread::scope(|scope| -> Result<()> {
                        let stop_on_exit = Stop(&stop);
                        let mut consumers = Vec::new();
                        for (lane, (sender, receiver)) in pairs.into_iter().enumerate() {
                            let stop = &stop;
                            scope.spawn(move || {
                                let mut audio = AudioFrame::builder()
                                    .sample_rate(48_000)
                                    .num_channels(2)
                                    .num_samples(block)
                                    .build()
                                    .unwrap();
                                let period = Duration::from_secs_f64(block as f64 / 48_000.0);
                                while !stop.load(Ordering::Acquire) {
                                    let next = Instant::now() + period;
                                    audio.set_timecode(Timecode::from_ticks(
                                        (epoch.elapsed().as_nanos() / 100) as i64,
                                    ));
                                    sender.send_audio(&audio);
                                    thread::sleep(next.saturating_duration_since(Instant::now()));
                                }
                            });
                            #[cfg(feature = "tokio")]
                            let tokio = &tokio;
                            consumers.push(scope.spawn(move || -> Result<()> {
                                let callback = Duration::from_micros(callback_us);
                                let values = match mode {
                                    #[cfg(feature = "tokio")]
                                    "tokio" => tokio.block_on(sample::<grafton_ndi::asynchronous::Tokio>(receiver, epoch, callback))?,
                                    #[cfg(feature = "smol")]
                                    "smol" => ::smol::block_on(sample::<grafton_ndi::asynchronous::Smol>(receiver, epoch, callback))?,
                                    _ => unreachable!(),
                                };
                                for (sample, value) in values.into_iter().enumerate() {
                                    eprintln!("audio,{mode},{lanes},{lane},{block},{callback_us},{sample},{value:.3}");
                                }
                                Ok(())
                            }));
                        }
                        for consumer in consumers {
                            consumer.join().unwrap()?;
                        }
                        drop(stop_on_exit);
                        Ok(())
                    })?;
                }
            }
        }
    }
    Ok(())
}
