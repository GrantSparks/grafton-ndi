# File API

The `file-io` Cargo feature adds the `grafton_ndi::file` module. It records NDI
sources to disk and plays recordings back as NDI sources, through the NDI File
library. This guide explains what the module needs, how a recording is
finished, and the threading rules of its types. The rustdoc of each type is the
reference.

## Requirements

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["file-io"] }
```

The File library is a separate native library that the NDI SDK installs beside
the runtime library. Nothing else in the crate depends on it, so its absence
never affects sending or receiving. `FileApi::open` finds it when it is first
needed:

```rust
use grafton_ndi::{
    file::{FileApi, FileError, FileRuntimeOptions},
    NDI,
};

fn main() -> grafton_ndi::Result<()> {
    let ndi = NDI::new()?;
    match FileApi::open(&ndi, &FileRuntimeOptions::default()) {
        Ok(file) => println!("File library: {:?}", file.library_path()),
        // The text lists every location tried and why each failed.
        Err(FileError::Unavailable(reason)) => println!("no File library: {reason}"),
        Err(error) => println!("{error}"),
    }
    Ok(())
}
```

Unless `FileRuntimeOptions::library_path` names an exact file, the first usable
library in this order is selected:

1. the directory of the selected NDI runtime library, where the SDK installs
   it;
2. the directory that contains the running executable;
3. the directory named by the `NDI_RUNTIME_DIR_V6` environment variable;
4. the platform loader's own search for the library name.

Like the NDI runtime, a File library that was selected stays selected and
loaded for the life of the process. A library that loads but cannot be used
also stays loaded. If the platform loader's own search then matches the
library name to that module, it returns the module again instead of looking
further. The error names the file. Remove or fix it, or pass
`FileRuntimeOptions::library_path`.

Which NDI runtime the File library works with depends on the platform:

| Platform | Rule |
| --- | --- |
| Linux, macOS, Android | The File library uses the Advanced SDK runtime. `FileApi::open` refuses it beside a Standard runtime before loading anything. Select the Advanced runtime; see [SDK editions](../README.md#sdk-editions). |
| Windows | The File library does not depend on the runtime's edition. The SDK ships it for 64-bit Windows only. |
| iOS, tvOS, visionOS | The File archive is linked at build time; see [Apple mobile targets](../README.md#apple-mobile-targets). |

## Probing a recording

`FileApi::probe` reads a recording's duration and track layout without
advertising anything on the network:

```rust
use grafton_ndi::file::FileApi;

fn describe(file: &FileApi) -> Result<(), Box<dyn std::error::Error>> {
    let info = file.probe("take_001.mkv")?;
    println!(
        "{:?} long, {} video and {} audio track(s)",
        info.duration().value().duration(),
        info.video_track_count(),
        info.audio_track_count(),
    );
    Ok(())
}
```

## Playing a recording

`FileApi::create_player` advertises an NDI source and returns a `FilePlayer`.
The source is advertised from creation until the player is dropped, whether or
not a file is playing, so receivers stay connected between files.

```rust
use grafton_ndi::file::{FileApi, PlayerEventKind, PlayerOptions};
use std::time::Duration;

fn play(file: &FileApi) -> Result<(), Box<dyn std::error::Error>> {
    let mut player = file.create_player(PlayerOptions::new("Replay")?)?;
    println!("advertising {}", player.advertised_source()?);
    player.start("take_001.mkv")?;
    loop {
        match player.poll_event(Duration::from_millis(500))? {
            Some(event) if event.kind() == PlayerEventKind::Ended => break,
            Some(event) => println!("{:?}", event.kind()),
            None => {}
        }
    }
    Ok(())
}
```

- `start` plays a file through the source; a file already playing is closed
  first. `stop` ends the current file only.
- `set_paused`, `set_looping`, `seek` and `seek_frame_approximate` control
  playback. `duration`, `position` and `source_timecode` report where it is.
- `poll_event` returns the player's events: file opened, first frame, looped,
  ended, file closed, and errors.
- Dropping the player stops playback and removes the source.

Example: `examples/file_play.rs`.

## Recording a source

`FileApi::create_recorder` returns a `FileRecorder`. The recorder subscribes to
the NDI source named in its `RecorderOptions` and stores the compressed data as
it arrives. It takes no frames from Rust.

```rust
use grafton_ndi::file::{
    FileApi, FinishStatus, RecorderOptions, RecordingTarget, RecordingTimecode, SourceSpec,
};
use std::time::Duration;

fn record(file: &FileApi) -> Result<(), Box<dyn std::error::Error>> {
    let options = RecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?);
    let mut recorder = file.create_recorder(options)?;
    recorder.start(RecordingTarget::new("take")?, RecordingTimecode::Preserve)?;

    // Take the recorder's events while it records.
    for _ in 0..20 {
        if let Some(event) = recorder.poll_event(Duration::from_millis(500))? {
            println!("{event:?}");
        }
    }

    match recorder.finish(Duration::from_secs(5))? {
        FinishStatus::Closed(report) => println!("closed {} file(s)", report.closed_files()),
        other => println!("not confirmed complete: {other:?}"),
    }
    Ok(())
}
```

A session runs from `start` until its last file is closed. It can span several
files: `rollover` starts a new file, `set_source` changes the source, and the
SDK starts a new file by itself when the options ask for it, for example at a
size limit. `RecorderOptions` also selects the tracks to record and how files
are named. `inject_metadata` writes a metadata frame into the recording.

`start` and `rollover` check that the target's directory exists and is
writable before they call the SDK, and return `FileError::Io` when it is not.
The session, if one is open, goes on. A recorder can start a new session after
any session has ended, including one the SDK ended by itself, for example at a
format change when the options disable the rollover.

### Finishing a recording

The SDK finalizes a file after a stop request and reports it with a
file-closed event. `finish` requests the stop, waits for that event and returns
a `FinishStatus`:

| Status | Meaning |
| --- | --- |
| `Closed` | Every file the session opened was reported finalized. |
| `TimedOut` | The wait ended first. Call `wait_finished` to keep waiting. |
| `Indeterminate` | The session ended, but its events do not show that every file was finalized. Waiting longer does not change this. |

Treat a recording as complete only when the status is `Closed`. Each status
carries a `RecordingReport` with the number of files closed, the number of
those that were closed before a first frame arrived (`empty_files`), error
events, write failures and lost events. A confirmed closure says that the SDK
finalized the files. It says nothing about frames dropped while recording, or
about the data having reached the disk; see
[Limits of the NDI 6.4 File library](#limits-of-the-ndi-64-file-library).

The SDK discards events that are not taken promptly, and a session that lost
events can no longer be confirmed closed. Call `poll_event` throughout a
recording; `finish` takes events itself while it waits.

Dropping a recorder whose session is still open stops it and waits for the
file to be finalized, for at most `RecorderOptions::with_drop_wait`
(`RecorderOptions::DEFAULT_DROP_WAIT` is five seconds). The drop cannot report
the outcome, so call `finish` when the result matters. `abandon` destroys the
recorder at once without waiting; the file of a session that had not ended may
be left truncated.

Example: `examples/file_record.rs`.

## Limits of the NDI 6.4 File library

These were observed with the NDI 6.4 File library on Linux. They lie inside the
library, and this crate cannot correct them.

- **A file reported closed is not proof that its data was written.** When the
  disk fills while a session is being stopped, a write comes up short, and the
  library still extends the file and reports it closed without an error.
  `FileApi::probe` then reports a final duration for a file whose data is not
  there. `FinishStatus::Closed` with no errors means that the container was
  finalized, not that every frame reached the disk. Check a recording that must
  be complete by reading it back.
- **The process does not survive a full disk during recording.** When a write
  fails while a session is recording, the library queues a write-failed event
  and an error event and then crashes in its own writer thread, which ends the
  process. Record only where enough space is certain.
- **A file can be closed with nothing in it.** The library opens a file as soon
  as a session or a rollover starts, and reports it closed even if no frame
  arrived: after a stop before the source delivered anything, a switch to a
  source that is not there, or a rollover while the source is lost.
  `RecordingReport::empty_files` counts these files.
- **A rollover the library cannot carry out ends the session.** The library
  accepts the request, fails to open the new file, closes the old one and
  stops. `rollover` checks the target's directory first, so the usual mistake
  is refused and the session goes on. Other causes, such as an exact file name
  that already exists, still end the session.

## Threads and async code

`FilePlayer` and `FileRecorder` are neither `Send` nor `Sync`. The SDK
documents no threading contract for them, so each stays on the thread that
created it. Every method calls into the SDK and can block, including
destruction; timeouts bound event waits only. The values they return are owned
and can be sent anywhere.

`AsyncFilePlayer` and `AsyncFileRecorder` keep each native object on a thread
of its own and expose its operations as futures that work with any executor.
Clones share one object.

```rust
use grafton_ndi::file::{
    AsyncFileRecorder, FileApi, FinishStatus, RecorderOptions, RecordingTarget,
    RecordingTimecode, SourceSpec,
};
use std::time::Duration;

async fn record(file: FileApi) -> Result<(), Box<dyn std::error::Error>> {
    let options = RecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?);
    let recorder = AsyncFileRecorder::create(file, options).await?;
    recorder
        .start(RecordingTarget::new("take")?, RecordingTimecode::Preserve)
        .await?;

    // ... record for as long as needed ...

    if let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(5)).await? {
        println!("closed {} file(s)", report.closed_files());
    }
    recorder.close().await?;
    Ok(())
}
```

The async recorder takes its events continuously, so its report stays current
without polling. Await `finish` to learn whether the recording is complete,
then await `close`; neither blocks the executor. Dropping the last clone
instead blocks the dropping thread until the object's thread has finished. For
a recorder that means stopping an open session, waiting up to the drop wait
for the file to be finalized, and destroying the recorder. The file is then
finalized as far as the SDK reported within that wait, but the outcome cannot
be reported.

## Errors

File operations return `FileResult<T>`, whose error is `FileError`:

| Variant | Meaning |
| --- | --- |
| `Unavailable` | The File library, or the part of it an operation needs, cannot be used in this process. The text says why. |
| `InvalidInput` | A value cannot be represented by the native API, for example a path that is not UTF-8. |
| `InvalidState` | The operation is not permitted in the current session state. |
| `Rejected` | The SDK refused an operation; the recorder's or player's events may say more. |
| `Protocol` | The SDK returned something that violates the expected contract. |
| `Core` | An operation of the core NDI API failed. |
| `Io` | A filesystem operation failed. |

## Running the tests

The File tests follow the crate's test policy. Those that need the File library
are skipped, with a message, when it cannot be used, and they make their own
recordings. To run them on Linux x86_64, build with the Advanced SDK and select
its runtime:

```sh
export NDI_SDK_DIR="/path/to/NDI Advanced SDK for Linux"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=file cargo test --features file-io
```

`GRAFTON_NDI_REQUIRE_RUNTIME=file` makes a test that would have skipped fail
instead. The tests that check how the File library is found use stand-in
libraries and need no SDK; they run on Linux only.

The File tests were run on Linux x86_64 against the File library of the NDI
6.4.0.2 SDK. The Windows and macOS rules above come from the SDK's libraries
and were not exercised by a test run.
