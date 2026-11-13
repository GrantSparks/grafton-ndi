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
//! seconds to record (10 by default), followed by optional `--groups Studio`,
//! `--ip 192.0.2.10`, or `--by-name`. A complete NDI name is selected exactly
//! through the configured finder and recorded at its discovered endpoint.
//! `--by-name` explicitly asks the File SDK to rediscover by logical name instead.
//! A source that contains no parenthesis,
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
    Finder, FinderOptions, Ndi,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(source), Some(prefix)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: file_record <NDI source name or address> <output prefix> [seconds]");
        std::process::exit(2);
    };
    let mut arguments = arguments.peekable();
    let seconds: u64 = if arguments
        .peek()
        .is_some_and(|value| !value.starts_with("--"))
    {
        arguments.next().unwrap().parse()?
    } else {
        10
    };
    let mut discovery = FinderOptions::new();
    let mut by_name = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--groups" => {
                discovery = discovery.groups(arguments.next().ok_or("--groups needs a value")?)
            }
            "--ip" => {
                discovery = discovery.extra_ips(arguments.next().ok_or("--ip needs a value")?)
            }
            "--by-name" => by_name = true,
            _ => return Err(format!("unknown option: {argument}").into()),
        }
    }

    if seconds == 0 {
        return Err("recording duration must be positive".into());
    }
    let ndi = Ndi::new()?;
    let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;
    println!("File library: {:?}", file.library_path());

    // NDI names read "MACHINE (Source)"; anything else is an address.
    let spec = if by_name {
        SourceSpec::new(&source)?
    } else if source.contains('(') {
        let finder = Finder::new(&ndi, &discovery)?;
        let selected = finder
            .find_source_by_name(&source, Duration::from_secs(10))?
            .ok_or_else(|| grafton_ndi::Error::NoSourcesFound {
                criteria: source.clone(),
            })?;
        println!("Selected {selected}");
        SourceSpec::try_from(&selected)?
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
