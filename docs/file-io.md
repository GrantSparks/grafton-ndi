# File API

The `file-io` Cargo feature adds the `grafton_ndi::file` module. It records NDI
sources to disk and plays recordings back as NDI sources, through the NDI File
library.

## Requirements

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["file-io"] }
```

The File library is a separate native library that the NDI SDK installs beside
the runtime library. It requires NDI 6.4. Sending and receiving do not depend on it. `FileApi::open` finds it when it is first
needed:

```rust
use grafton_ndi::{
    file::{FileApi, FileRuntimeOptions},
    Error, Ndi,
};

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::new()?;
    match FileApi::open(&ndi, &FileRuntimeOptions::default()) {
        Ok(file) => println!("File library: {:?}", file.library_path()),
        // The text lists every location tried and why each failed.
        Err(Error::RuntimeLoadFailed(reason)) => println!("no File library: {reason}"),
        // The selected runtime is one the File library cannot accompany.
        Err(Error::RuntimeConflict(reason)) => println!("File library refused: {reason}"),
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
nothing else. A relative path fails with `Error::InvalidInput`, and a path other
than that of the library already selected with `Error::RuntimeConflict`.

Like the NDI runtime, a File library that was selected stays selected and
loaded for the life of the process. A library that loads but cannot be used
also stays loaded. If the platform loader's own search then matches the
library name to that module, it returns the module again instead of looking
further. The error names the file. Remove or fix it, or pass
`FileRuntimeOptions::library_path`.

Loading either native library executes its constructors before the wrapper
checks symbols, even if the candidate is rejected. For services or elevated
processes, make the first selection use absolute paths under application-owner
control. Both options try only that file, with no search fallback:

```rust
use grafton_ndi::{Ndi, RuntimeOptions, file::{FileApi, FileRuntimeOptions}};
# fn main() -> grafton_ndi::Result<()> {
let ndi = Ndi::with_options(&RuntimeOptions::new()
    .library_path("/opt/my-service/ndi/libndi_advanced.so.6"))?;
let file = FileApi::open(&ndi, &FileRuntimeOptions::new()
    .library_path("/opt/my-service/ndi/libndifile.so.6"))?;
println!("Runtime: {:?}; File library: {:?}",
    ndi.runtime().info().library_path(), file.library_path());
# Ok(())
# }
```

Use the native names for your platform. Dependencies still follow platform
loader rules and also need trusted locations. If using default File discovery,
trust the runtime's companion directory as well as every other searched
directory. A later explicit selection cannot undo code already executed by an
earlier load. See [runtime selection](setup.md#selecting-a-runtime).

Which NDI runtime the File library works with depends on the platform:

| Platform | Rule |
| --- | --- |
| Linux, macOS, Android | The File library uses the Advanced SDK runtime. `FileApi::open` refuses it beside a Standard runtime with `Error::RuntimeConflict`, before loading anything. Select the Advanced runtime; see [SDK editions](setup.md#sdk-editions). |
| Windows | The File library does not depend on the runtime's edition. The SDK ships it for 64-bit Windows only. |
| iOS, tvOS, visionOS | The File archive is linked at build time, in place of the core archive, when the NDI SDK for Apple supplies one; without it `FileApi::open` returns `Error::RuntimeLoadFailed`. See [Apple mobile targets](setup.md#apple-mobile-targets). |

### What the selected library provides

The File library's entry points come in two families, each a `FileFamily`:
`Playback`, for `FileApi::probe` and the players, and `Recording`, for the
recorders. A library is selected when it provides at least one of them
completely, and `FileApi::status` or `FileApi::supports` says which:

```rust
use grafton_ndi::file::{FileApi, FileFamily};

fn can_record(file: &FileApi) -> bool {
    file.supports(FileFamily::Recording)
}
```

An operation that needs a family the selected library lacks fails, before it
calls anything, with `Error::UnsupportedCapability` whose `feature` is
`RuntimeFeature::File` with that family, whose `reason` is
`CapabilityUnavailable::SymbolUnavailable` and whose `detail` names the first
entry point the library does not export. The library stays selected, so this
lasts for the life of the process. The runtime's own `Capabilities` do not know
what the File library provides, which is loaded apart from it: they report
every family as `CapabilityUnavailable::SeparateLibrary`.

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
    let mut player = file.create_player(&PlayerOptions::new("Replay"))?;
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

Carry an explicit discovery selection into a recorder with
`SourceSpec::try_from(&selected)`. A source with an address becomes a native
URL/address-only request, preserving its exact IP or URL text; an unaddressed
source becomes a name request. This avoids the File SDK's precedence rule, where
a nonempty name overrides a supplied URL. Selection uses the original finder's
scope:

```rust
use grafton_ndi::{Finder, FinderOptions, file::{RecorderOptions, SourceSpec}};
let finder = Finder::new(&ndi, &FinderOptions::new()
    .groups("Studio").extra_ips("192.168.1.100"))?;
let selected = finder.find_source_by_name("STUDIO (Camera 2)", Duration::from_secs(5))?
    .ok_or_else(|| grafton_ndi::Error::NoSourcesFound { criteria: "Camera 2".into() })?;
let endpoint = SourceSpec::try_from(&selected)?;
let mut recorder = file.create_recorder(&RecorderOptions::new(endpoint.clone()))?;
recorder.set_source(&endpoint)?; // the same conversion applies to subsequent requests
```

Endpoint pinning follows the selected interface/address and can become stale.
Use `SourceSpec::new(selected.name())` when the deliberate policy is logical
name rediscovery that may follow an address change. Neither policy silently
falls back to the other after a failed connection. Empty or NUL-containing
selected representations are refused before native mutation, including an
invalid address on an otherwise named source. Async create and `set_source`
commands copy the same validated selection before queueing it. The
[`file_record` example](../examples/file_record.rs) demonstrates both policies.

A session runs from `start` until its last file is closed. It can span several
files: `rollover` starts a new file, `set_source` changes the source, and the
SDK starts a new file by itself when the options ask for it, for example at a
size limit. `RecorderOptions` also selects the tracks to record and how files
are named. `inject_metadata` writes a metadata frame into the recording.

Before they call the SDK, `start` and `rollover` work out the file that the
filename pattern names for the target. Its directory must exist and, on Unix,
be writable; with an exact file name the file must not exist yet. Otherwise
they return `Error::Io`, of kind `AlreadyExists` for an exact file that
exists, and the session, if one is open, goes on. A pattern can name a
directory of its own, such as `{prefix}/{index:03}.mkv`; that directory is the
one checked.

The methods of `FileApi`, `FileRecorder` and `FilePlayer` borrow the options,
targets and sources they are given, so a value stays with the caller when the
call fails and can be corrected and passed again. The async handles borrow them
too, and send a copy to the thread that does the work.

`start` returns `Error::InvalidState(StateError::PreviousSessionOpen)`
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
  keyframe (`rollover_size`), whether a format change starts a new file
  (`format_change_rollover`), how files are named (`naming`, with a
  `FileNaming`) and whether the source's proxy is recorded alongside its
  full-quality video (`proxy`). Options are checked when the recorder is
  created (`create_recorder`, `reset`, or creating an `AsyncFileRecorder`),
  against each other as well, so the order of these calls does not matter.
  `PlayerOptions` and `AsyncFileOptions` are likewise checked when the player
  or the async handle is created.

### Exact file names

By default files are indexed (`FileNaming::Indexed`): the target `take`
becomes `take_001.mkv`, and each new file of the session takes the next free
index. `FileNaming::Exact`, or a `FileNaming::Pattern` without `{index}` or
`{index:N}`, names the file exactly instead. One target then names one file, so
the session cannot move to a new file by itself:

- `start` and `rollover` refuse a target whose file exists at preflight with
  `Error::Io(AlreadyExists)`, before calling the SDK. `rollover` to a new
  target works.
- A format change of the source closes the file and ends the session, because
  format-change rollover is off for an exact name unless set.
  `RecordingReport::stopped_by_sdk` then reports that the SDK ended it.
- `format_change_rollover(true)` or a nonzero `rollover_size` is
  refused with `Error::InvalidInput` when the recorder is created
  (`create_recorder`, `reset`, or creating an `AsyncFileRecorder`).
- `set_source` during a session returns
  `Error::InvalidState(StateError::ExactFileName)` without calling the SDK,
  and the session goes on. Between sessions it works as for any recorder.

Use an indexed pattern for a recording that must survive a format change or a
change of source.

The preflight checks are not filesystem reservations. A file or symlink created
between the check and the native open can make `start` fail in the SDK. A
`rollover` can return `Ok(())` before that later open fails, closes the prior
file and ends the session; observe open-failed/error events and the final
`RecordingReport`, including `stopped_by_sdk`.

On Linux x86-64 File SDK 6.4.0.2, native exact-name creation uses exclusive
open: racing regular files, existing-target symlinks and dangling symlinks are
preserved. This is verified only for that platform and release; Windows,
macOS, Android and other releases have not been verified. The wrapper does not
reserve the final path, since that would prevent the SDK's exclusive open.
Use a recorder-controlled output directory when uninterrupted recording or
destination containment matters. Parent directories are not pinned against
replacement, and this API makes no containment promise for mutable parents.


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

`Closed` means that the SDK reported the files finalized. `RecordingReport::stopped_by_sdk`
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
file to be finalized, for at most `RecorderOptions::drop_wait`
(`RecorderOptions::DEFAULT_DROP_WAIT` is five seconds). The drop cannot report
the outcome, so call `finish` when the result matters. `abandon` destroys the
recorder at once without waiting and returns its report; the file of a session
that had not ended may be left truncated.

Example: `examples/file_record.rs`.

## Limits of the NDI 6.4 File library

These were observed with the NDI 6.4 File library on Linux, except where a
platform is named. They lie inside the library, and this crate cannot correct
them.

- **On macOS a file that a source switch closes can be left unfinalized.** The
  library reports the file closed, but it holds only its header, and
  `FileApi::probe` finds no final duration. Files that a rollover or a stop
  closes are finalized.

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
created it. Native SDK calls can block, including destruction; timeouts bound event
waits only. The values they return are owned
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

async fn record(file: &FileApi) -> Result<(), Box<dyn std::error::Error>> {
    let options = RecorderOptions::new(SourceSpec::new("STUDIO (Camera 1)")?);
    let recorder = AsyncFileRecorder::create(file, &options).await?;
    recorder
        .start(&RecordingTarget::new("take")?, RecordingTimecode::Preserve)
        .await?;

    // ... record for as long as needed ...

    match recorder.finish(Duration::from_secs(5)).await? {
        FinishStatus::Closed(report) => println!("closure report: {report:?}"),
        other => println!("closure unconfirmed: {other:?}"),
    }
    recorder.close().await?;
    Ok(())
}
```

The async recorder consumes SDK events continuously. Use `next_event(timeout)`
for application notifications; dropped notifications do not change the recording
report. Configure bounded queues with `AsyncFileOptions`. A full command queue
returns `QueueFull`; see the [async API reference](https://docs.rs/grafton-ndi/latest/grafton_ndi/file/struct.AsyncFileRecorder.html)
for queue limits, cancellation and shutdown behavior.

Await `finish` and inspect its `FinishStatus` and report, then await `close`.
`close` ends the object for all clones without blocking the executor. Later
operations return `StateError::Closed`; a failed worker returns `OwnerFailed`.

**Dropping the last clone blocks** until its worker exits. A recorder attempts
to stop and waits up to `drop_wait` for closure events, then destroys the native
recorder. Drop cannot report or guarantee finalization. The wait budget does
not bound SDK stop or destruction calls. Await `close()` before dropping the
last handle on an executor thread.

## Errors

File operations return `grafton_ndi::Result<T>`, whose error is the crate's
`Error`, as every other operation does. These are the variants the File API
reports:

| Variant | Meaning |
| --- | --- |
| `RuntimeLoadFailed` | No usable File library was found; the text lists every location tried and why each failed. Nothing was selected, so `FileApi::open` can be called again. |
| `UnsupportedCapability` | The selected File library does not provide the `FileFamily` an operation needs: `feature` is `RuntimeFeature::File` with the family, `reason` is `SymbolUnavailable`, and `detail` names the first entry point missing. `FileApi::status` reports it in advance. |
| `RuntimeConflict` | The File library cannot accompany the selected runtime, or `FileRuntimeOptions::library_path` names a library other than the one already selected. |
| `InvalidInput` | A value cannot be represented by the native API, for example a path that is empty or is not UTF-8, or options that are out of range or conflict, which are refused when the player, recorder or async handle is created. The text names what the value is, such as the file path of a player or probe, or a recording target. |
| `InvalidCString` | A string for the SDK contains a NUL: a path, a source name or URL, a player's name or groups, or a filename pattern. |
| `InvalidState` | The operation is not permitted in the current state. The `StateError` it carries says which, for example `PreviousSessionOpen` for `start` during an open session, `ExactFileName` for `set_source` during a session with an exact file name, `WaiterPending` for a second event wait on an async handle, or `Closed` and `OwnerFailed` for an async handle that has been closed or whose thread failed. `StateError::is_retryable` is true only for `WaiterPending`, which can succeed later without anything else changing. |
| `QueueFull` | An async handle's command queue is full (`QueueLimit::FileCommands`), or so is the queue of async probes (`QueueLimit::FileProbes`). The same call can succeed once queued work has run. A request to stop is refused only while another waits beyond the full queue. |
| `NativeOperationFailed` | The SDK refused an operation, which `operation` names; the recorder's or player's events may say more. |
| `UnexpectedSdkResult` | The SDK returned something that violates the expected contract. |
| `Io` | A filesystem operation failed, or a thread for an async handle or for async probes could not be started. |

## Validation

See [SDK validation](sdk-validation.md) for tested platforms and
[CONTRIBUTING.md](../CONTRIBUTING.md) for local test commands.
