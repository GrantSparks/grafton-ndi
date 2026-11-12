# File API

The `file-io` Cargo feature adds the `grafton_ndi::file` module. It records NDI
sources to disk and plays recordings back as NDI sources, through the NDI File
library. This guide explains what the module needs, how a recording is
finished, and the threading rules of its types. The rustdoc of each type is the
reference.

The module is a supported part of the crate and follows its semantic
versioning like the rest of the API. Its errors name what went wrong by type
(`FileError`, and `StateError` for an operation the current state does not
allow), its public structures can gain fields without a breaking change, and
its enums are `#[non_exhaustive]`, so later SDK releases can be supported in
3.x minor versions.

## Requirements

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["file-io"] }
```

The File library is a separate native library that the NDI SDK installs beside
the runtime library. It first ships with the NDI 6.4 SDK; the 6.1.1 and 6.3.2
SDKs do not include it. Nothing else in the crate depends on it, so its absence
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

The file is named `libndifile.so.6` on Linux, `libndifile.dylib` on macOS,
`libndifile.so` on Android and `Processing.NDI.Lib.File.x64.dll` on 64-bit
Windows. `FileRuntimeOptions::library_path` takes an absolute path and tries
nothing else. A relative path, or a path other than that of the library already
selected, fails with `FileError::Core`.

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
| iOS, tvOS, visionOS | The File archive is linked at build time, in place of the core archive, when the NDI SDK for Apple supplies one; without it `FileApi::open` returns `FileError::Unavailable`. See [Apple mobile targets](../README.md#apple-mobile-targets). |

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
    let mut player = file.create_player(&PlayerOptions::new("Replay")?)?;
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
    let mut recorder = file.create_recorder(&options)?;
    recorder.start(&RecordingTarget::new("take")?, RecordingTimecode::Preserve)?;

    // Take the recorder's events while it records.
    for _ in 0..20 {
        if let Some(event) = recorder.poll_event(Duration::from_millis(500))? {
            println!("{event:?}");
        }
    }

    match recorder.finish(Duration::from_secs(5))? {
        FinishStatus::Closed(report) if report.stopped_by_sdk() => {
            println!("finalized, but the SDK ended the session early: {report:?}")
        }
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

Before they call the SDK, `start` and `rollover` work out the file that the
filename pattern names for the target. Its directory must exist and, on Unix,
be writable; with an exact file name the file must not exist yet. Otherwise
they return `FileError::Io`, of kind `AlreadyExists` for an exact file that
exists, and the session, if one is open, goes on. A pattern can name a
directory of its own, such as `{prefix}/{index:03}.mkv`; that directory is the
one checked.

The methods of `FileApi`, `FileRecorder` and `FilePlayer` borrow the options,
targets and sources they are given, so a value stays with the caller when the
call fails and can be corrected and passed again. The async handles take them
by value, because their work runs on another thread.

`start` returns `FileError::InvalidState(StateError::PreviousSessionOpen)`
while a session is recording or stopping. A recorder learns that a session ended only from the events it takes,
so keep calling `poll_event`, or call `finish`, until `state()` is
`RecorderState::Closed` or `RecorderState::Indeterminate`. A new session can
then start, including after a session the SDK ended by itself, for example at a
format change when the options disable the rollover.

Other recorder operations:

- `state()` follows the events taken so far, and `report()` returns the
  `RecordingReport` of the current session at any time.
- `request_stop` asks the SDK to stop without waiting.
- `last_accepted_source()` is the source the SDK last accepted, and
  `reported_current_file()` the file named by the events taken so far. The
  SDK's own getters for these return strings that its recording thread can
  invalidate, so the crate does not call them; these values can lag behind
  the SDK.
- `RecorderOptions` sets a size after which a new file starts at the next
  keyframe (`with_rollover_size`), whether a format change starts a new file
  (`with_format_change_rollover`), the file name pattern (`exact_filename`,
  `with_pattern`) and whether the source's proxy is recorded alongside its
  full-quality video (`with_proxy`). The options are checked against each
  other when the recorder is created, so the order of these calls does not
  matter.

### Exact file names

By default files are indexed: the target `take` becomes `take_001.mkv`, and
each new file of the session takes the next free index. `exact_filename()`, or
a `with_pattern` pattern without `{index}` or `{index:N}`, names the file
exactly instead. One target then names one file, so the session cannot move to
a new file by itself:

- `start` and `rollover` refuse a target whose file exists, so nothing is ever
  overwritten. `rollover` to a new target works.
- A format change of the source closes the file and ends the session, because
  format-change rollover is off for an exact name unless set.
  `RecordingReport::stopped_by_sdk` then reports that the SDK ended it.
- `with_format_change_rollover(true)` or a nonzero `with_rollover_size` is
  refused with `FileError::InvalidInput` when the recorder is created
  (`create_recorder`, `reset`, or creating an `AsyncFileRecorder`).
- `set_source` during a session returns
  `FileError::InvalidState(StateError::ExactFileName)` without calling the SDK,
  and the session goes on. Between sessions it works as for any recorder.

Use an indexed pattern for a recording that must survive a format change or a
change of source.

### Finishing a recording

The SDK finalizes a file after a stop request and reports it with a
file-closed event. `finish` requests the stop, waits for that event and returns
a `FinishStatus`:

| Status | Meaning |
| --- | --- |
| `Closed` | Every file the session opened was reported finalized. |
| `TimedOut` | The wait ended first. Call `wait_finished` to keep waiting. |
| `Indeterminate` | The session ended, but its events do not show that every file was finalized. Waiting longer does not change this. |

The wait covers taking the closure's event, not the stop, which the 6.4 SDK
carries out before `NDIlib_file_recorder_stop` returns. Events already queued
are taken even when the wait is over, so `finish(Duration::ZERO)` reports
`Closed` when the SDK finalized the file during the stop.

`Closed` means that the files were finalized. `RecordingReport::stopped_by_sdk`
says whether the recording ran until you stopped it: it is true when the
session ended without a stop requested through `request_stop`, `finish`,
`abandon` or a drop, for example after a new file could not be opened or at a
format change that does not roll over. Such a session is finalized but shorter
than intended, and its `error_events` usually say why.

Treat a recording as complete only when the status is `Closed`. Each status
carries a `RecordingReport` with the number of files closed, the number of
those that were closed before a first frame arrived (`empty_files`), error
events, write failures and lost events. A confirmed closure says that the SDK
finalized the files. It says nothing about frames dropped while recording, or
about the data having reached the disk; see
[Limits of the NDI 6.4 File library](#limits-of-the-ndi-64-file-library).

The SDK discards events that are not taken promptly, and a session that lost
events can no longer be confirmed closed. Call `poll_event` throughout a
recording; `finish` takes events itself while it waits. A malformed event does
not end that wait: what it showed about the session is recorded as `poll_event`
records it, and an event that could not be read at all makes the outcome
`Indeterminate`. Once the wait has begun, `finish` and `wait_finished` always
return a `FinishStatus`, and a malformed event cannot cut short the wait of a
drop either.

If the SDK stops reporting events and `wait_finished` keeps timing out, `reset`
gives up on the session. It creates a new idle recorder from the
`RecorderOptions` it is given, abandons the old native recorder and returns the
old session's `RecordingReport`, which stays unconfirmed; the old file may be
truncated. The replacement has its own event queue, so late events from the old
session cannot affect a new one, and session numbers keep increasing. If the
replacement cannot be created, `reset` returns the error and the old recorder
stays as it was.

Dropping a recorder whose session is still open stops it and waits for the
file to be finalized, for at most `RecorderOptions::with_drop_wait`
(`RecorderOptions::DEFAULT_DROP_WAIT` is five seconds). The drop cannot report
the outcome, so call `finish` when the result matters. `abandon` destroys the
recorder at once without waiting and returns its report; the file of a session
that had not ended may be left truncated.

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
  stops. `rollover` checks the file the pattern names first: a missing or
  unwritable directory, and an exact file name that already exists, are
  refused and the session goes on. A file that cannot be opened for another
  reason, or one created after the check, still ends the session, which
  `RecordingReport::stopped_by_sdk` then reports.

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
without polling. `next_event(wait)` delivers them to the application, including
those `finish` takes while it waits, with a malformed event as
`FileError::Protocol`; a handle keeps up to 256 undelivered events, discards
the oldest beyond that and counts the discarded ones in
`notification_dropped_count`. A discarded notification does not change the
recording report. `AsyncFileOptions` and `create_with_options` change the queue
sizes.

`next_event(wait)` returns `Ok(None)` when `wait` runs out, even while the
object's thread is blocked in a command, because one timer thread shared by
every async handle in the process keeps the deadlines; it starts with the first
wait that needs it. If that thread cannot be started, the wait fails with
`FileError::Io`, and the next wait tries again. A pending `next_event` fails
with `FileError::InvalidState(StateError::Closed)` as soon as the object's
thread stops taking events, without waiting for a recorder's drop wait.

Await `finish` to learn whether the recording is complete, then await `close`;
neither blocks the executor. `close` ends the object for every clone, and later
calls on other clones fail with `FileError::InvalidState(StateError::Closed)`.
`AsyncFileRecorder::reset` replaces the native recorder as `FileRecorder::reset`
does.

**Dropping the last clone blocks the dropping thread** until the object's
thread has finished. For a recorder that means stopping an open session,
waiting up to the drop wait for the file to be finalized, and destroying the
recorder; for a player, destroying it and removing its source. This is
deliberate: a file is finalized by the time the drop returns, even when the
drop happens as the program exits, although the outcome cannot be reported.
Code that must not block, such as a task on an async executor, awaits `close()`
before letting the last clone go; once the object is closed, dropping its clones
does not block.

## Errors

File operations return `FileResult<T>`, whose error is `FileError`. The enum
is `#[non_exhaustive]`:

| Variant | Meaning |
| --- | --- |
| `Unavailable` | The File library, or the part of it an operation needs, cannot be used in this process. The text says why. |
| `InvalidInput` | A value cannot be represented by the native API, for example a path that is not UTF-8. |
| `InvalidState` | The operation is not permitted in the current state. The `StateError` it carries says which, for example `PreviousSessionOpen` for `start` during an open session, `ExactFileName` for `set_source` during a session with an exact file name, or `QueueFull` and `Closed` for an async handle whose command queue is full or that has been closed. `StateError::is_retryable` is true only for `QueueFull` and `WaiterPending`, which can succeed later without anything else changing. |
| `Rejected` | The SDK refused an operation; the recorder's or player's events may say more. |
| `Protocol` | The SDK returned something that violates the expected contract. |
| `Core` | An operation of the core NDI API failed, or `FileApi::open` was given a relative library path or one that conflicts with the library already selected. |
| `Io` | A filesystem operation failed. |

## Running the tests

The File tests follow the crate's test policy. Those that need the File library
are skipped, with a message, when it cannot be used, and they make their own
recordings. To run them on Linux x86_64, point the runtime search at the
Advanced SDK's runtime; a `file-io` build tries the Advanced library first:

```sh
export NDI_RUNTIME_DIR_V6="/path/to/NDI Advanced SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=file cargo test --locked --config ci/offline.toml --features file-io
```

`GRAFTON_NDI_REQUIRE_RUNTIME=file` makes a test that would have skipped fail
instead. The tests create NDI senders and receivers; `--config ci/offline.toml`
confines them to loopback networking, and needs `bubblewrap`. The tests that
check how the File library is found use stand-in libraries and need no SDK;
they run on Linux only and need `rustc` on the `PATH`.

The File tests were run on Linux x86_64 against the File library of the NDI
6.4.0.2 SDK, beside the Advanced 6.4.0.2 runtime. The rules above for Windows,
macOS, Android and the Apple mobile targets come from the SDK's libraries and
the crate's code, and were not exercised by a test run.
