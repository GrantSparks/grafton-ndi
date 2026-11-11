# File recording and playback

Enable the optional `file-io` Cargo feature to use `grafton_ndi::file`. It does not
require the separate Rust `advanced` feature. The File module remains available
when compiling against older SDK headers; opening it requires a compatible native
File artifact. Existing core operations do not depend on that artifact.

The recorder subscribes to an NDI source and writes its media to a file.
The player publishes a file as an NDI source. These APIs do not accept Rust video
buffers or expose a general packet-file reader. Use `FileApi::probe` to inspect
tracks and duration without advertising a source.

## Selecting the runtime

`FileApi::open` validates the complete native operation group and retains both its
module and the core runtime lease. It creates no player or recorder. Shared builds
locate File beside the selected core library by default; an absolute
`FileRuntimeOptions::with_library_path` can select another exact File module.
A module-load failure, missing symbol, edition mismatch and unavailable build
artifact have distinct capability diagnostics, reported through
`FileError::Core(Error::UnsupportedCapability { .. })`.

The supplied POSIX shared File libraries require the matching Advanced core.
Standard and unverified Advanced selections are rejected before File is loaded;
the loaded dependency must then resolve to the exact selected core module.
Loading a second edition beside Standard is not a supported workaround.
Windows x64 uses the matching distribution's File DLL; Windows x86 has no supplied
File artifact. Apple mobile static profiles select the combined File/core archive
as the sole provider when it is available and `file-io` is enabled. Those builds
do not accept a shared File path. An older static SDK without File keeps its core
provider and reports File unavailable.

For a POSIX Advanced shared installation:

```rust,no_run
use grafton_ndi::{NDI, RuntimeOptions, SdkEdition};
use grafton_ndi::file::{FileApi, FileResult, FileRuntimeOptions};

fn open_file_api() -> FileResult<FileApi> {
    let options = RuntimeOptions::default().edition(SdkEdition::Advanced);
    let ndi = NDI::with_options(&options)?;
    FileApi::open(&ndi, &FileRuntimeOptions::default())
}
```

Select the runtime before another part of the process initializes NDI. An explicit
shared core path is configured with `RuntimeOptions::library_path`; static linkage
is chosen at build time. See [SDK validation](sdk-validation.md) for target setup
and evidence, and [the CI guide](../ci/README.md) for artifact/toolchain profiles.

## Playback

```rust,no_run
use std::path::Path;
use grafton_ndi::file::{FileApi, FilePlayer, FileResult, PlayerOptions};

fn open_player(api: &FileApi, path: &Path) -> FileResult<FilePlayer> {
    let info = api.probe(path)?; // No advertised source.
    println!("{} video tracks", info.video_track_count());
    let mut player = api.create_player(PlayerOptions::new("File playback")?)?;
    // The source is already advertised, even before a file starts.
    player.start(path)?;
    Ok(player)
}
```

`stop` closes playback while retaining the advertised source. Drop or `close`
removes the native player. Looping is instance state and survives file changes;
pause applies to the current file, has no effect before a file opens, and resets
on a new start. Events and source/format snapshots are owned Rust values.

`FilePosition` uses unsigned 100 ns file-relative ticks. `SourceTimecode` preserves
signed editorial timecode separately. GOP seeking can resume at a preceding
keyframe; `seek_frame_approximate` also involves frame-rate conversion. Neither
method promises frame-exact seeking. `FileDuration::is_final` distinguishes a
final duration from a provisional value for a growing file.

## Recording and finalization

```rust,no_run
use std::path::Path;
use grafton_ndi::file::{
    FileApi, FileRecorder, FileResult, RecorderOptions, RecordingTarget,
    RecordingTimecode, SourceSpec,
};

fn start_recording(
    api: &FileApi,
    source: SourceSpec,
    prefix: &Path,
) -> FileResult<FileRecorder> {
    let mut recorder = api.create_recorder(RecorderOptions::new(source))?;
    recorder.start(RecordingTarget::new(prefix)?, RecordingTimecode::Preserve)?;
    Ok(recorder)
}
```

Choose `SourceSpec::new` for an NDI source name or `SourceSpec::from_url` for a
direct address. Recorder creation prepares the object; `start` begins recording.
The default tracks are video, audio and metadata, with the proxy enabled when
available. Track selection, filename pattern, format-change rollover, proxy and
size threshold are configured through `RecorderOptions` before creation.
`RecordingTimecode::Preserve` retains source timecode;
`RecordingTimecode::GenerateFrom` creates a session timecode beginning at the
supplied signed tick value and continuing across rollovers/source changes.

The default filename pattern is `{prefix}_{index:03}.mkv`; the SDK searches for
an unused index. `exact_filename()` uses the supplied path and fails when that
file already exists. The wrapper never deletes an existing file or creates its
parent directory. `RecordingTarget` freezes relative paths against the current
directory at construction and rejects non-UTF-8 paths and embedded NUL bytes.
Names and native paths must be losslessly representable as UTF-8. Where native
input-copy timing is unspecified, submitted path/source strings remain owned
until the player or recorder is destroyed.

Poll events during recording to retain lifecycle evidence and avoid native queue
loss. Stop requests are asynchronous. Use `finish` to request stop and wait, or
`request_stop` followed by `wait_finished`. Each finite wait borrows the recorder;
timeout or an error does not consume it.

```rust,no_run
use std::time::Duration;
use grafton_ndi::file::{FileRecorder, FileResult, FinishStatus};

fn finish_once(recorder: &mut FileRecorder) -> FileResult<FinishStatus> {
    // The caller retains this recorder for another wait if the budget expires.
    recorder.finish(Duration::from_secs(5))
}
```

| Outcome | Meaning and next action |
| --- | --- |
| `Closed(report)` | A matching final `FileClosed` event was observed, the queue was drained and native recording stopped. Inspect error/write-failure counts and capture statistics before treating the recording as successful. |
| `TimedOut(report)` | The wait budget expired. Keep the recorder and call `wait_finished` again. |
| `Indeterminate(report)` | Missing or inconsistent lifecycle evidence prevents certification. Keep observing if needed; do not treat native inactivity alone as confirmed closure. |

`RecordingReport::closure_confirmed` establishes observed container closure, not
lossless capture or filesystem `fsync` durability. Source disconnection does not
by itself end a session. The completion ledger tracks session boundaries, file
rollovers, native event drops and owned path observations before decoding other
event fields. Bounded event strings preserve invalid UTF-8 and truncation evidence.

Dropping a recorder calls native stop/destruction but cannot certify finalization;
`abandon` makes that same choice explicit. Native calls, including destruction,
can block. A timeout bounds event waiting, not arbitrary SDK processing. A session
with indeterminate evidence cannot simply be restarted on the same recorder.

Two native pointer getters remain deliberately unavailable:

- `native_source()` returns `UnsupportedCapability` because background-stop
  invalidation lacks a documented safe copy boundary. `last_accepted_source()`
  reports the last accepted request, not an authoritative live source query.
- `native_current_file()` returns the same ownership-contract restriction because
  automatic rollover can invalidate the pointer. `reported_current_file()` is an
  owned-event observation that may lag or be truncated.

These restrictions also apply to the asynchronous adapter. All 35 File operations
have public representations, but these two getters are not implemented as native
calls; the library does not claim complete usable SDK surface while they remain
restricted.

## Asynchronous owners

`AsyncFilePlayer` and `AsyncFileRecorder` create and retain each native object on
one dedicated thread. Their futures work with any Rust executor and add no Tokio
or smol dependency. Synchronous owners remain thread-affine.

```rust,no_run
use std::path::PathBuf;
use grafton_ndi::file::{AsyncFilePlayer, FileApi, FileResult, PlayerOptions};

async fn start_async(api: FileApi, path: PathBuf) -> FileResult<AsyncFilePlayer> {
    let player = AsyncFilePlayer::create(api, PlayerOptions::new("Async playback")?).await?;
    player.start(path).await?;
    Ok(player)
}
```

Operation methods enqueue immediately, before their returned `FileOperation` is
polled. Dropping an ordinary operation before its execution gate cancels that
work; after the gate it only abandons the result. Accepted stop/finish/close
requests persist. Cancelling a finish wait retains the owner and resumes native
event pumping. Its wait budget includes command-queue delay.

`AsyncFileOptions` bounds command and notification queues. A full command queue
rejects work. A full notification queue discards the oldest user notification and
increments `notification_dropped_count`; the recorder's internal ledger has already
processed that event. Native `dropped_event_count` is separate and can invalidate
finalization evidence. Only one pending `next_event` waiter is allowed per owner;
its timeout starts on first poll.

Dropping the last handle requests cleanup without joining the owner thread. Await
`close` to observe native destruction; closing invalidates all clones. For a
recorder, call `finish` and inspect its report first when finalized output is
required. Cleanup cannot promise a deadline that native destruction does not offer.

## Validation evidence

Current Linux x86_64 evidence includes pure ownership/value/ABI tests against
6.3.2 Standard and 6.4 headers, an independent C/Rust File layout comparison, and
native 6.4 Advanced probe/player, async player and recorder/finalize/probe tests in
a network namespace. Standard POSIX rejection before module loading is also tested.
The native test definitions are in [sdk_tests.rs](../src/file/sdk_tests.rs).

Required hosted native suites on Linux x64, Windows x64 and macOS ARM64 include
matching File tests or explicit unavailability assertions. Other supported
profiles retain ABI/link or packaging checks; those checks do not certify native
File behavior. Consult the exact revision's [validation results](sdk-validation.md)
for completed execution. Support is limited to SDK/toolchain combinations
obtainable and usable on GitHub-hosted CI; specialized BSP artifacts without those
prerequisites remain inventory only and unsupported. Codec/peripheral behavior and
older untested 6.x artifacts remain separate evidence gaps. No SDK installation or
feature flag grants redistribution or product entitlement by itself.
