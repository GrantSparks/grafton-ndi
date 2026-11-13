//! Example: recording an NDI source to disk with the File API.
//!
//! Records a source for a number of seconds, printing the recorder's events as
//! they arrive, then stops and waits for the SDK to confirm the file is
//! finalized.
//!
//! Run with:
//! `cargo run --example file_record --features file-io -- "MACHINE (Source)" take 10`
//!
//! Arguments: the source's NDI name, the output prefix (the default naming
//! writes `take_001.mkv`, `take_002.mkv`, ...), and optionally the number of
//! seconds to record (10 by default). A source that contains no parenthesis,
//! such as `192.168.1.20:5961`, is taken as an address instead of a name, which
//! records it without discovering it first.
//!
//! With the default options the recorder waits a few seconds at start-up for
//! the source's low-bandwidth proxy stream, so a very short recording can be
//! empty; `RecorderOptions::proxy(false)` starts at once.
//!
//! The File library ships with the NDI SDK, beside the runtime library. On
//! Linux and macOS it needs the Advanced SDK runtime.

use std::{
    env,
    time::{Duration, Instant},
};

use grafton_ndi::{
    file::{
        FileApi, FileRuntimeOptions, FinishStatus, RecorderEventKind, RecorderOptions,
        RecordingTarget, RecordingTimecode, SourceSpec,
    },
    Ndi,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(source), Some(prefix)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: file_record <NDI source name or address> <output prefix> [seconds]");
        std::process::exit(2);
    };
    let seconds = arguments.next().map_or(Ok(10), |text| text.parse())?;

    if seconds == 0 {
        return Err("recording duration must be positive".into());
    }
    let ndi = Ndi::new()?;
    let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
    println!("File library: {:?}", file.library_path());

    // NDI names read "MACHINE (Source)"; anything else is an address.
    let spec = if source.contains('(') {
        SourceSpec::new(&source)?
    } else {
        SourceSpec::from_url(&source)?
    };
    let mut recorder = file.create_recorder(&RecorderOptions::new(spec))?;
    recorder.start(&RecordingTarget::new(&prefix)?, RecordingTimecode::Preserve)?;
    println!("Recording {source} for {seconds} s...");

    // Take events throughout: the SDK discards those left waiting, and a
    // session that lost events cannot be confirmed closed.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        let Some(event) = recorder.poll_event(Duration::from_millis(250))? else {
            continue;
        };
        match event.kind() {
            RecorderEventKind::Started | RecorderEventKind::Rollover => {
                println!("writing {}", event.path().to_string_lossy());
            }
            RecorderEventKind::FileClosed => {
                println!("closed {}", event.path().to_string_lossy());
            }
            kind => println!("{kind:?} {}", event.message().to_string_lossy()),
        }
    }
    if let Some(stats) = recorder.stats()? {
        println!(
            "{} video and {} audio packets, {} bytes",
            stats.video_frames(),
            stats.audio_frames(),
            stats.bytes_written()
        );
    }

    // A recording is complete only when the SDK reports its last file closed.
    match recorder.finish(Duration::from_secs(10))? {
        FinishStatus::Closed(report) => {
            println!(
                "Finalized {} file(s); {} error event(s) during the session.",
                report.closed_files(),
                report.error_events()
            );
            if report.error_events() > 0 {
                return Err(format!(
                    "recording reported {} error event(s)",
                    report.error_events()
                )
                .into());
            }
            if report.stopped_by_sdk() {
                println!("The SDK ended the session before it was asked to stop.");
            }
        }
        other => return Err(format!("recording was not confirmed complete: {other:?}").into()),
    }
    Ok(())
}
