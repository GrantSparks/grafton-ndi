//! Native File idle CPU measurement; Linux thread CPU excludes other SDK threads.

use super::*;

struct Counted {
    recorder: FileRecorder,
    polls: usize,
    empty: usize,
    errors: usize,
}

impl Owner for Counted {
    type Event = RecorderEvent;
    fn poll_native(&mut self) -> Result<Option<Self::Event>> {
        self.polls += 1;
        let result = self.recorder.poll_native();
        match &result {
            Ok(None) => self.empty += 1,
            Err(_) => self.errors += 1,
            _ => {}
        }
        result
    }
}

fn thread_cpu() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: this initialized timespec is writable for the synchronous call;
    // libc supplies the target ABI and CLOCK_THREAD_CPUTIME_ID reads this thread.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
        0
    );
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// The owner loop before a017d01, retained here only as the CPU baseline.
fn previous_owner<H: Owner>(
    owner: &mut H,
    receiver: &Receiver<Job<H>>,
    control: &Control,
    events: &EventQueue<H::Event>,
) {
    while !control.shutdown.load(Ordering::Acquire) {
        for _ in 0..16 {
            match owner.poll_native() {
                Ok(Some(event)) => events.push(Ok(event)),
                Ok(None) => break,
                Err(error) => {
                    events.push(Err(error));
                    break;
                }
            }
        }
        match receiver.recv_timeout(TICK) {
            Ok(job) => {
                if control.shutdown.load(Ordering::Acquire) {
                    break;
                }
                job(owner);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[test]
#[ignore = "manual native File owner idle thread-CPU comparison"]
fn native_file_idle_resources() -> Result<()> {
    let Some((ndi, api)) = crate::test_support::file_api() else {
        return Ok(());
    };
    eprintln!(
        "runtime,{} {:?},file={:?}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        api.library_path()
    );
    for round in 0..5 {
        for index in 0..2 {
            let previous = (round + index) % 2 == 0;
            // Create, use and destroy the native owner on this test thread.
            // An unstarted recorder has no output file or incoming media.
            let recorder = api.create_recorder(&RecorderOptions::new(SourceSpec::from_url(
                "127.0.0.1:59999",
            )?))?;
            let mut owner = Counted {
                recorder,
                polls: 0,
                empty: 0,
                errors: 0,
            };
            let events = EventQueue::new(16);
            let control = Control::new();
            let (sender, receiver) = mpsc::channel();
            // Warm native poll state outside the measurement.
            for _ in 0..10 {
                owner.poll_native()?;
                thread::sleep(TICK);
            }
            owner.polls = 0;
            owner.empty = 0;
            owner.errors = 0;
            let finish = thread::spawn(move || {
                thread::sleep(Duration::from_secs(1));
                drop(sender);
            });
            let wall = Instant::now();
            let cpu = thread_cpu();
            if previous {
                previous_owner(&mut owner, &receiver, &control, &events);
            } else {
                run_owner(&mut owner, &receiver, &control, &events);
            }
            let cpu = thread_cpu() - cpu;
            let wall = wall.elapsed();
            finish.join().unwrap();
            assert_eq!(owner.errors, 0);
            assert_eq!(
                owner.empty, owner.polls,
                "the measured native workload must be idle"
            );
            assert!(lock(&events.0).queue.is_empty());
            eprintln!(
                "file-idle,{previous},{round},{},{},{:.6},{:.6},{:.6}",
                owner.polls,
                owner.empty,
                wall.as_secs_f64() * 1000.0,
                cpu.as_secs_f64() * 1000.0,
                cpu.as_secs_f64() / wall.as_secs_f64() * 100.0
            );
        }
    }
    Ok(())
}
