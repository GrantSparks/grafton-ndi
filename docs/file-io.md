# File API

The `file-io` feature records NDI sources to disk and plays recordings as NDI
sources. Enable it independently of `advanced`:

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["file-io"] }
```

## Requirements

The File API uses a separate native library. See [setup](setup.md#sdk-editions)
for runtime selection and [platform support](setup.md#platforms) for supported
targets. On Linux, macOS and Android the wrapper requires an Advanced runtime
for File operations; the `advanced` Cargo feature is not required. The File ABI
is included, so enabling `file-io` alone does not generate bindings.

Open it with `FileApi::new(&ndi, &FileApiOptions::default())`. On targets
that load libraries dynamically, discovery tries the selected runtime's
directory, the executable's directory and an absolute `NDI_RUNTIME_DIR_V6`.
Windows then expands absolute `PATH` entries and tries a System32-only name
lookup. macOS tries `@rpath/<name>` and absolute `DYLD_LIBRARY_PATH` entries.
Linux and Android retain their native loader search (`rpath`,
`LD_LIBRARY_PATH` and system locations). Windows and macOS never add the
working directory to this search. Relative or empty environment directory
entries on Windows/macOS, and a relative `NDI_RUNTIME_DIR_V6` on any platform,
are recorded as refused and never opened. On macOS, any relative or empty
`DYLD_LIBRARY_PATH` entry refuses the whole default search before opening a
library: dyld applies these overrides even to companion and `@rpath` requests.
Explicit paths retain dyld's own policy. In unrestricted macOS processes,
dyld can substitute `DYLD_LIBRARY_PATH` even for an absolute path; the crate
refuses a substitute with `LoadRefusal::ResolvedToAnotherFile`. Native
dependencies still follow platform loader rules.
`FileApiOptions::library_path` selects one absolute file without
fallback. A successful selection lasts for the process; a later conflicting
selection returns `RuntimeConflict`. Use trusted library locations, including
their dependencies, as described in [runtime selection](setup.md#selecting-a-runtime).

### What the selected library provides

Check `FileApi::status` or `supports`, or call `require_family`, for
`FileFamily::Playback` and `FileFamily::Recording`. Missing family symbols produce
`Error::UnsupportedCapability { capability: Capability::File(family), refusal }`;
loading the core runtime does not establish
File availability. See the [File API reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/index.html)
for loading errors and the full family contract.

## Probing a recording

`FilePlayer::probe(&api, path)` reads duration and track information without advertising
an NDI source. The [`file_play` example](../examples/file_play.rs) demonstrates
probing before playback.

## Playing a recording

```sh
cargo run --example file_play --features file-io -- take_001.mkv "Replay"
```

The example advertises `Replay`, plays the recording once, prints events and
reports video counters. `FilePlayer::new(&api, &FilePlayerOptions::new(name))`
advertises from creation until drop,
including between files. Use `start` and `stop` for files, `set_paused` and
`set_looping` for playback control, and `seek` or `seek_frame_approximate` for
positioning. Poll events to observe progress and errors. Exact units and seek
semantics are in [`FilePlayer`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/struct.FilePlayer.html).

## Recording a source

```sh
cargo run --example file_record --features file-io -- "MACHINE (Camera 1)" take 10
```

The [`file_record` example](../examples/file_record.rs) discovers that source,
records for ten seconds, drains events and inspects the finish report. Default
indexed naming produces `take_001.mkv`, then successive files as needed. Use
its `--groups` and `--ip` options to scope discovery, or `--by-name` to ask the
File SDK to rediscover the source by name.

Create the recorder with `FileRecorder::new(&api, &FileRecorderOptions::new(spec))`.
Player and recorder options are cloneable, comparable values. Player groups
accept a list, for example `.groups(["Studio"])`.

Choose the source policy deliberately:

- `SourceSpec::try_from(&selected)` pins a discovered address when present,
  otherwise its name. The address may become stale.
- `SourceSpec::new(name)` requests name-based rediscovery.
- `SourceSpec::from_url(address)` requests a direct endpoint.

These policies do not silently fall back to one another on connection failure.
The recorder subscribes to the source itself; it does not consume frames from
Rust. Options choose tracks, proxy recording, naming and rollover. Keep polling
events throughout a synchronous recording: state and closure confirmation depend
on the events consumed. An open or stopping session prevents another `start`.
`rollover` requests another file, `set_source` changes the input, and
`inject_metadata` accepts `MetadataSource`, including captured metadata.

### Exact file names

Use indexed naming for recordings that must span source or format changes.
`FileNaming::Exact`, or a pattern without an index, limits a target to one file.
For an exact name, the wrapper rejects format-change rollover, a nonzero
rollover size, and source changes during a session. See
[`FileRecorderOptions`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/struct.FileRecorderOptions.html).

In custom patterns, `{index}` and `{index:N}` must be in the file name.
Directories may use literal text and `{prefix}`, for example
`{prefix}/{index:03}.mkv`. Patterns such as `{prefix}/{index}/clip.mkv` are
refused when the recorder is created: every rollover must stay in the directory
checked at preflight.

`start` and `rollover` preflight the output directory and reject an existing
exact target. This does not reserve the path or pin parent directories against
replacement. Use a recorder-controlled output directory. A successful rollover
request still needs events and the final report to establish its outcome.

### Finishing a recording

Call `finish(timeout)` and inspect both its status and `RecordingReport`:

| Status | Action |
| --- | --- |
| `Closed` | The SDK reported every opened file finalized. Check errors, write failures, empty files and `stopped_by_sdk` for an early end. |
| `TimedOut` | Use `wait_for_finish` to continue waiting. |
| `Indeterminate` | Closure could not be established from the events; waiting longer does not repair the report. |

`finish(timeout)` requests the stop and reports that bounded completion attempt,
including `FinishStatus::TimedOut(report)`. A later
`wait_for_finish(timeout) -> Result<Option<FinishStatus>>` only waits: `None`
means its deadline expired, while `Some(Closed(report))` or
`Some(Indeterminate(report))` carries a final outcome. It never returns
`Some(TimedOut(...))`. After `None`, inspect `recorder.report()` for the current
session snapshot and keep waiting or choose recovery.

The timeout bounds the event wait, not the native stop call. Lost events can
prevent confirmation. If recovery is needed, `reset` replaces the native
recorder and returns the old session's report; `abandon` destroys it without
waiting. Either can leave an unfinished file.

Drop requests a stop and waits for closure events within one `drop_wait`
budget (five seconds by default), including known pending files after an
indeterminate outcome, but cannot report the outcome. The budget starts at
drop, or an async close's submission, and includes the native stop. The stop
can itself outlast the budget; after it returns, only the remaining time is
available for closure events. Synchronous `FileRecorder::finish` instead
measures its event wait from the stop's return, giving closure its full
requested wait. Until those files close, a new start
returns `PreviousSessionOpen`; poll for closure or use `reset` to abandon them.
Use explicit finishing when the result matters. See
[`FileRecorder`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/struct.FileRecorder.html)
for state transitions, error conditions and recovery.

If another async clone calls `close` while a submitted `finish` or
`wait_for_finish` is queued or running,
the wait uses the earlier of its own deadline and `drop_wait` measured from
the close's submission, including the native stop. A settled session reports `Closed` or `Indeterminate`
(wrapped in `Some` for `wait_for_finish`); an unsettled one returns
`InvalidState(Closed)` if that budget ends first. Neither `TimedOut` from
`finish` nor `None` from `wait_for_finish` would be appropriate for a handle
that cannot be waited on again. Destruction
uses only the remainder of the close's budget after an earlier finish deadline;
the finish never shortens the time available to close.

## Limits of the NDI 6.4 File library

`Closed` confirms the SDK's closure events. It does not establish that every
expected frame was captured or that bytes are durable on disk. Provision output
space, inspect the report and read back recordings whose completeness matters.
[Previously reported native failures](https://github.com/GrantSparks/grafton-ndi/blob/ad56e37c7306088e46e8575c604304d7b0d187b2/docs/file-io.md#limits-of-the-ndi-64-file-library)
include process crashes on write failure and closed events for incomplete files;
no archived run log is available here to qualify their platform or frequency.
Plan recording as a fallible operation, and isolate it in a worker process when
a native crash must not end the service.
Native calls can block inside the vendor library; wrapper timeouts cannot
interrupt them. Test recording and recovery with your selected runtime,
platform and storage.

## Threads and async code

Synchronous players and recorders stay on their creating thread; they are
neither `Send` nor `Sync`. `AsyncFilePlayer` and `AsyncFileRecorder` each own a
native object on a worker thread and expose futures usable with any executor.
Clones share that object; no Tokio or smol feature is required for these handles.

Operations are lazy until first polled or explicitly `submit()`ted. Dropping an
unpolled operation does nothing. Ordinary queued work can be cancelled before
it starts; submitted stop, finish and close requests continue after their futures
are dropped. Native work already started runs to completion. Timeout budgets
include command queue time after submission.

The async recorder drains native events continuously. `next_event` supplies
bounded application notifications; dropped notifications do not alter its
recording report. Configure queue limits with `AsyncFileOptions` and handle
`QueueFull` when submitting commands.
Pass `&AsyncFileOptions` to `create_with_options`. Statistics name their units:
player counters are `*_frames_sent`, recorder counters are `*_frames_written`,
and the recorder reports `frames_dropped_by_writer` separately. Recorder written
counters count muxed packets, rather than decoded audio samples or image data;
receiver, pipeline and writer drops remain distinct.

While `finish` or `wait_for_finish` waits for closure, the owner runs commands
from every clone between event bursts. Status queries remain available, and
several waits can observe the same session with independent deadlines. Native
calls, including the initial stop, can still block the owner. A successful
`reset` abandons the old native recorder and ends its pending finish waits with
`InvalidState(Closed)`; a failed reset preserves those waits.

Async `wait_for_finish` returns `FileOperation<Option<FinishStatus>>`, with the
same `None`/`Some` distinction. After an expired wait, await
`recorder.report()` to inspect the current report. Async `finish` retains
`FileOperation<FinishStatus>` and its `TimedOut(report)` outcome.

Await `finish`, inspect its report, then await `close` to shut down all clones
without blocking the executor. **Dropping the last clone blocks** until its
worker exits. Its `drop_wait` budget includes the native stop, which can
itself outlast that budget; native destruction can also take longer.
When an executor callback drops the last clone on a File owner, timer or probe
thread, it requests shutdown without waiting. This lets those threads keep
delivering notifications; await `close` when cleanup must have completed.
See [`AsyncFileRecorder`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/struct.AsyncFileRecorder.html)
for cancellation, queue and shutdown contracts.

## Errors

Library selection reports `RuntimeLoadFailed(failure)` with
`failure.library() == LibraryKind::File` and typed search attempts. Inspect
`LoadRefusal` and `failure.loaded_any()` before treating a load failure as an
uninstalled library. `RuntimeConflict(failure)` exposes `kind()` and typed
library paths to distinguish a `FileNeedsAdvancedRuntime` conflict from a different selected library, linked
implementation or foreign runtime in the global symbol scope.

File operations use the crate's `Error`. Distinguish unavailable libraries or
families (`RuntimeLoadFailed`, `UnsupportedCapability`), conflicting selections
(`RuntimeConflict`), invalid arguments or state, queue saturation (`QueueFull`),
native failures and filesystem errors. Method rustdoc describes which errors
leave an existing session running and which reports require recovery.

## Validation

See [testing from source](../ci/README.md) for runtime checks. For thumbnails, use the
[combined File/image example](image-encoding.md#thumbnails-from-a-recording).
