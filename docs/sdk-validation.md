# SDK integration validation

This document states what the tests of the current version cover, the
measured behavior of the NDI SDK that the crate relies on, and how to repeat
the tests on your own machine. The
[setup guide](setup.md#supported-ndi-versions-and-platforms) summarizes it. The
crate supports NDI 6 runtimes and Rust 1.88 or later.

## What the tests cover

The test suite runs with `cargo test --locked` on the platforms and runtimes
listed under [Platform coverage](#platform-coverage). Every run against a real
runtime uses `--config ci/offline.toml`, which on Linux and macOS confines each
test executable to loopback networking, and sets `GRAFTON_NDI_EXPECTED_RUNTIME`,
`GRAFTON_NDI_EXPECTED_VERSION` and `GRAFTON_NDI_EXPECTED_EDITION`, so each test
that loads a runtime checks that it has loaded exactly the intended file,
version and edition.

### Runs with one SDK

Each row of the `sdk` CI job runs the suite against one runtime, with all
features and with no default features; the
[CI guide](../ci/README.md#the-sdk-job) lists the rows.
`GRAFTON_NDI_REQUIRE_RUNTIME` states what the row's runtime provides, so that
a test cannot pass by skipping, and each row visibly skips the Advanced or
File operations that its build and runtime cannot provide. Without a runtime,
the tests that need one report skips. The ignored tests are manual benchmarks
and a slow stress test.

`tests/receiver_video_only.rs` sends in real time, clocked 30 fps video with
1/30 s of audio per frame. A sender that sends as fast as its loop turns, some
500 times real time, on a machine with every core busy, makes the runtime drop
a receiver's connection within the first second and not restore it within 30
seconds, even after the load and the sending stop.

### Runs that cross versions

"Advanced headers" below is the Advanced SDK that `NDI_SDK_DIR` names for a
build with the `advanced` feature, whose Advanced-only headers that build
reads; "None" is a build that reads no SDK. The core bindings are always the
vendored 6.4.0.2 ones. These are the crossed runs the `sdk` CI job makes on
its Linux x64 Advanced 6.4.0.2 row, all with every feature enabled. In each, a
second process on the "peer" runtime sends video, audio and metadata to a
receiver on the run's runtime, and `tests/sdk_peers.rs` checks the decoded
pixels, nonzero audio samples, frame timecodes and metadata. Both processes
are confined to loopback, and each checks the identity of the runtime it
loaded.

| Advanced headers | Runtime | Peer sending | `GRAFTON_NDI_REQUIRE_RUNTIME` |
| --- | --- | --- | --- |
| Advanced 6.4.0.2 | Standard 6.1.1 | Advanced 6.4.0.2 | `1` |
| Advanced 6.4.0.2 | Advanced 6.3.2 | Advanced 6.4.0.2 | `advanced` |
| None | Advanced 6.4.0.2 | Standard 6.1.1 | `1,file,discovery-server` |
| Advanced 6.3.2 | Advanced 6.4.0.2 | Advanced 6.3.2 | `advanced,file,discovery-server` |

So 6.1.1 and 6.4 exchange media in both directions, as do Advanced 6.3.2 and
6.4, and Advanced builds run against runtimes of another version in both
directions.

To require the peer test locally, set `GRAFTON_NDI_PEER_RUNTIME` to the second
SDK's exact library path, `GRAFTON_NDI_PEER_VERSION` to its four-component
version, and `GRAFTON_NDI_PEER_EDITION` to `standard` or `advanced`; the peer
must be a different version from the runtime the tests load. With these set, a
missing peer, an identity mismatch, a delivery failure or a failed shutdown of
the child process fails the test. Without them it reports a skip.

### What each runtime provides

The Linux x86_64 runtimes report their versions as `6.1.1` (for 6.1.1),
`6.3.2.0` and `6.4.0.2` at the end of their version text. A build with the
Advanced SDK 6.4.0.2 headers reports these capabilities for them:

| Capability | Standard 6.1.1 | Standard 6.3.2 | Advanced 6.3.2 | Standard 6.4.0.2 | Advanced 6.4.0.2 |
| --- | --- | --- | --- | --- | --- |
| `Planar444Video`, `ReceiveBest444`, `ReceiveVideoOnly` | `RuntimeTooOld` | `RuntimeTooOld` | `RuntimeTooOld` | Available | Available |
| `ReceiveDelay`, `SdkTimestamp` | `SymbolUnavailable` | `SymbolUnavailable` | `SymbolUnavailable` | Available | Available |
| `ConnectedSourceName` | `SymbolUnavailable` | Available | Available | Available | Available |
| `SenderAdvertising`, `ReceiverAdvertising`, `SenderDiscovery`, `ReceiverDiscovery` | `SymbolUnavailable` | `RuntimeTooOld` | `RuntimeTooOld` | Available | Available |
| `AdvancedSenderAdvertiserConfiguration`, `AdvancedReceiverAdvertiserConfiguration`, `AdvancedSenderListenerConfiguration`, `AdvancedReceiverListenerConfiguration` | `WrongEdition` | `WrongEdition` | `RuntimeTooOld` | `WrongEdition` | Available |
| Every other Advanced capability, including `Passthrough` with each of the 15 presets | `WrongEdition` | `WrongEdition` | Available | `WrongEdition` | Available |
| The other seven capabilities | Available | Available | Available | Available | Available |
| `FileApi::open` | Refused: Standard runtime | Refused: Standard runtime | No File library | Refused: Standard runtime | Available |

An operation whose capability is unavailable returns
`Error::UnsupportedCapability` before any native call is made. The 6.1.1 and
6.3.2 SDKs include no File library.

### Image encoding

`tests/image_color_sdk.rs` pins the SDK's color conversion: BT.709 at 720×576,
1920×1080 and 3840×2160 whatever the tag, tags passed through, repeated
chroma, RGB sent as BT.709 limited range, a PQ tag reaching an RGB receiver,
and FrameSync frames. `tests/video_padding_sdk.rs` encodes UYVY and P216
frames in place from the SDK's padded rows. See the
[image encoding guide](image-encoding.md) and its
[performance notes](image-encoding-performance.md).

### Reads past the end of sent video

The x86 builds of the SDK read past the last pixel of many UYVY and UYVA
frames they send. The SDK's documentation gives a UYVY frame as its line
stride times its height, asks for no alignment and no padding, and asks only
that widths be even; its own examples allocate exactly that. The builds for
64-bit ARM Linux read every frame exactly.

#### x86-64

Progressive frames, by size:

| Runtimes | Formats | Sizes | Result |
| --- | --- | --- | --- |
| Standard 6.1.1, 6.3.2 and 6.4.0.2; Advanced 6.3.2 and 6.4.0.2 | UYVY | 640×480, 720×480, 720×576, 854×480, 1280×720, 1366×768, 1920×1080, 1918×1080, 1080×1920, 640×1080, 642×32, 3840×2160 | The same on every runtime. No read past 640×480, 1280×720, 1920×1080 and 3840×2160. 4 bytes past 720×480, 720×576 and 1918×1080, 12 past 640×1080, 16 past 1080×1920, 20 past 854×480 and 1366×768, 28 past 642×32 |
| The same | UYVA | The same | The same on every runtime. 5 bytes past 854×480, 7 past 720×480, 720×576 and 642×32, 8 past 1366×768, 12 past 640×1080; none past the others |
| The same | P216, PA16, NV12, I420, YV12, BGRA, BGRX, RGBA, RGBX | The same | No read past the frame |
| Standard and Advanced 6.4.0.2 | NA48, NB48, NA4G, NB4G | Eight, from 18×32 to 3840×2160 | No read past the frame |
| Standard 6.4.0.2 | UYVY | 605: every even width to 200 at heights of 32 and 480, and 135 wider ones to 4096 at heights of 32, 480 and 1080 | Reads past 443 of them, 28 bytes at most. Whenever the width is not a multiple of 16, by at least the bytes that would complete the row's last 16 pixels. At 231 of the 393 sizes whose width is a multiple of 16, by up to 12 |
| Standard 6.4.0.2 | UYVA | The same 605 | Reads past 244 of them, 14 bytes at most |

The scan type changes the answer. With Standard 6.4.0.2, in bytes past the
frame, for progressive, interlaced and single-field frames:

| Size | UYVY | UYVA |
| --- | --- | --- |
| 1920×1080 | 0, 4, 12 | 0, 5, 14 |
| 1280×720 | 0, 0, 12 | 0, 0, 14 |
| 3840×2160 | 0, 0, 0 | 0, 0, 0 |
| 720×480 | 4, 4, 4 | 7, 7, 4 |
| 720×576 | 4, 4, 8 | 7, 7, 14 |
| 720×486 | 4, 4, 12 | 7, 7, 13 |
| 640×480 | 0, 0, 12 | 0, 0, 14 |
| 642×32 | 28, 28, 28 | 7, 7, 7 |

Standard 6.1.1 and 6.3.2 and Advanced 6.4.0.2 give the same figures at the
four of these sizes measured with them. Interlaced and single-field UYVY
frames of 51 widths from 2 to 3840, and 32, 480 and 1080 rows high, are read
at most 28 bytes past.

A height that is not a multiple of four is read further when the frame is
interlaced or a single field. At 1920×1082, 1920×1086, 2560×1442, 3840×2162
and 26 other sizes, all larger in area than 1920×1080, an interlaced UYVY
frame is read to the end of the row after next, two line strides past the
frame, on every runtime measured; single fields of 2560×1083 and 3840×1083
are read one row past. No progressive frame is, no frame whose height is a
multiple of four, and no UYVA frame of those sizes.

Three more facts bear on what a program sees:

- The reads happen on every frame, whether or not a receiver is connected.
- Where a frame ends within its page matters. On the five runtimes, a frame
  whose width is a multiple of 16 and that ends exactly on a page boundary is
  read exactly, while a 720×480 frame that ends elsewhere in a page is read 4
  bytes past. The figures above are for frames that end near a page boundary,
  so each is the furthest read measured and not a bound.
- What the SDK reads is compressed into the picture. A 642×32 UYVY frame
  followed by bytes of 16, and the same frame followed by bytes of 235,
  arrive with 12 samples different, by up to 3, all in the last two columns
  of the last eight rows.

#### 64-bit ARM

None of the Linux builds for 64-bit ARM, Standard 6.1.1, 6.3.2 and 6.4.0.2
and Advanced 6.4.0.2, reads past a frame. With Standard 6.4.0.2 that holds
for UYVY and UYVA frames of 38 widths, six heights and the four scan types,
and for six other formats.

#### 32-bit x86

The 32-bit x86 Linux build of Standard 6.4.0.2 reads a progressive 642×32
UYVY frame exactly, where the x86-64 builds read past it, and
`tests/send_tail_room_sdk.rs` checks that it does. It is measured at no other
size, so the crate keeps the x86 room for it.

The same runtime returns a 3840×2160 UYVY frame with the chroma inside flat
areas off by up to 41 code values, and does not give two receivers of one
stream the same picture; the luma, and frames of 720×576 and 1920×1080, are
intact. `tests/image_color_sdk.rs` leaves 3840×2160 out on 32-bit x86.

#### What the crate does

`PixelFormat::send_tail_room` is the room the crate gives the SDK after a
frame: 64 zero bytes for UYVY and UYVA, none for the other formats, and none
at all on 64-bit ARM Linux. An interlaced or single-field UYVY or UYVA frame
whose height is not a multiple of four gets two line strides more. An owned
frame's allocation holds the room. `Sender::send_video` packs active rows of
borrowed UYVY/UYVA frames into reusable staging with a zeroed tail by default
where that room is nonzero, regardless of supplied padding or tail room.
`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail(true)` permits
direct sending when sufficient initialized bytes follow the last pixel;
`readable_after` alone never opts in. Native reads of row padding can also
affect received pixels, so staging copies active rows only. On other
architectures and operating systems the crate takes the x86 figures.

Earlier measurements compared sending a UYVY frame from a buffer that ends
with it against direct sending of the same frame with room: 0.15 ms longer at 1280×720, 0.28 ms
at 1920×1080 and 1.1 ms at 3840×2160 with the 6.3.2 and 6.4.0.2 runtimes, in
a release build on Linux x86-64: 18%, 15% and 54% of the send.

The final packed staging path was benchmarked on 2026-11-13 in a release
build with Rust 1.99.1 on Linux x86-64, over 200 frames after warming the
reused allocation. These are wrapper staging times per frame, excluding SDK
encoding, rather than a comparison of total sends:

| Format | Resolution | Packed source | Source with 4 padding bytes per row |
| --- | --- | --- | --- |
| UYVY | 1280×720 | 0.114 ms | 0.112 ms |
| UYVY | 1920×1080 | 0.262 ms | 0.257 ms |
| UYVY | 3840×2160 | 0.890 ms | 1.241 ms |
| UYVA | 1280×720 | 0.178 ms | 0.187 ms |
| UYVA | 1920×1080 | 0.390 ms | 0.383 ms |
| UYVA | 3840×2160 | 1.509 ms | 2.275 ms |

Reproduce with
`cargo test --release --lib benchmark_borrowed_yuv_staging -- --ignored --nocapture`. Unit tests verify no allocations after staging warms.
The received-picture regression varies earlier-row padding, last-row padding,
trailing slice bytes and separately asserted initialized room, for both formats
and interlaced layouts. The guard-page test retains explicit direct-send and
single-field coverage.

`tests/send_tail_room_sdk.rs` sends UYVY and UYVA frames of nine sizes and the
four scan types from memory that ends at an inaccessible page, with exactly
the crate's room and with none, through every video send; and it pins what
the bytes after a frame do to the picture, as room, after a buffer and
vouched for, with the SDK's reading of them. With the room cut to one row, or
to 24 bytes, or the copy disabled, its first test ends in a segmentation
fault on x86-64. Under Valgrind Memcheck, with Standard 6.3.2 and 6.4.0.2 on
x86-64, neither that test nor `tests/video_padding_sdk.rs`, sending from
vectors that end with the frame, reads or writes outside an allocation,
apart from four reports in glibc's dynamic loader with 6.3.2. The first also
draws reports of uninitialised values, every one of them in memory the SDK
allocated.

On the five x86-64 runtimes, the audio sends, planar and interleaved, of 1 to
16 channels and 1 to 1,920 samples, and the V210, P216 and audio conversion
utilities read and write nothing outside their buffers.

## Receiver counter and FrameSync lifecycle probes

The loopback regression tests
`tests/receiver_drop_totals.rs` and
`tests/framesync_reception_lifecycle.rs` use externally paced 48 kHz stereo
audio in 480-sample packets. The first leaves an AudioOnly receiver's raw
queue unread until drops exceed half its received total. It checks that the
public received total includes frames subsequently dropped and that the
drop percentage uses that total. It asserts progress and a bounded deadline,
not a particular native queue capacity. The lifecycle test attaches a
FrameSync, requests fixed 48 kHz mono output through the borrowing path, and
checks startup, zero-valued input, pause, explicit disconnect, reconnect,
resumed input and sender disappearance. It never raw-captures audio beside
FrameSync. Its video check compares retained output with the separate native
received-video counter; video output retention is an observation, not a
portable promise.

Earlier isolated native probes on Linux x86_64 observed the same counter
relationship and audio lifecycle on Standard 6.3.2.0, Standard 6.4.0.2 and
Advanced 6.4.0.2. With a full receiver queue, a representative sample was
350 audio frames received, 285 dropped and 65 queued; capacity and exact
counts are implementation observations. Format knowledge persisted through
pause and explicit disconnect in those probes, and the received total reset
on explicit disconnect. The test does not require either behavior universally.
Standard 6.4.0.2 also retained a FrameSync video output frame after input
stopped. These loopback probes do not qualify a camera or venue network.

The repository regressions passed under the offline runner on Linux x86_64
with the following runtime identities:

| Runtime | Queue-overflow test | FrameSync audio and retained-video tests |
| --- | --- | --- |
| Standard 6.3.2.0 | Pass | Pass |
| Standard 6.4.0.2 | Pass | Pass |
| Advanced 6.4.0.2 | Pass | Pass |

Run the repository tests under the isolated runner with
`cargo test --config ci/offline.toml --all-features --test receiver_drop_totals --test framesync_reception_lifecycle`.
Set `NDI_RUNTIME_DIR_V6`, `NDI_SDK_DIR` and the expected identity variables
as described above for each runtime. A missing runtime follows the usual
skip/required-runtime convention. The runtime's version, edition and platform
are printed by each audio regression.

## Compatibility safeguards and additional runtime checks

Which Advanced operations a build can perform follows from the declarations
generated from the installed headers. A family of functions that the headers
lack disables only that family, and types or receive presets that older headers
lack do not stop them compiling. The build script and the runtime tables take
the families from one registry, `src/sys/advanced_tables.rs`. The coverage test
requires every Advanced function the installed headers declare to belong to a
family or to be listed as deliberately not wrapped. `tests/advanced_headers.rs`
builds the crate, with warnings denied, against synthetic partial headers and
checks the capabilities that build reports. That tests the fallback.

Further tests against 6.4 check a real lossless HEVC 4:4:4 frame, including
every sample of its three full-resolution planes; audio suppression by both
video-only bandwidth settings, with an ordinary receiver as a control; AVSync
following changes in video size, frame rate, audio sample rate and channel
count while preserving nonzero samples; and both Genlock clocks following a
local source and falling back to paced system clocks. The KVM boundary test
checks failed release, retry on drop, touch coordinates and clipboard
ownership using stand-in functions.

A FrameSync and raw audio capture on its receiver compete for the receiver's
audio, which is why `Receiver<FrameSyncAttached>` has no audio capture.
Measured on Linux x86_64 over loopback with Standard 6.3.2 and 6.4, with a
clocked sender of 30 fps video, each frame with 1600 samples of 48 kHz stereo
audio, and a FrameSync pulling 480 samples every 10 ms: without raw capture,
every frame reaches the FrameSync and it returns no silence. With raw audio
capture on the attached receiver, interleaved with the pulls or on a second
thread, each audio frame reaches exactly one of the two, and the FrameSync
returns silence for 65 to 90 per cent of the samples it pulls. Every metadata
frame still reaches the attached receiver's own metadata capture.

`FrameSync` is `Sync`, although neither its header nor the SDK documentation
of 6.1.1, 6.3.2 or 6.4 says whether one FrameSync may be used from several
threads at once. That rests on measurement on Linux x86_64 over loopback. In
the runtimes of all three versions, video capture, audio capture and the video
free take the instance's exclusive lock, the audio free returns its buffer
through atomic operations, and the audio queue depth is read without the lock.
Under helgrind, four threads making every `&FrameSync` call on one FrameSync
of a live source conflict only in the unlocked queue-depth read and the atomic
audio free, on 6.3.2 and 6.4. Sixteen threads making the same calls on each of
the three runtimes receive no torn video frame, no invalid audio sample and no
error. `tests/framesync_capture_boundary.rs` repeats a short version of the
multi-thread check against every runtime the tests use.

### Queued video submission

`tests/extended_lifetimes.rs` uses the existing fake runtime's immediate
completion callbacks to retain cloned pixel and compressed frames together,
then collect and resubmit them. It checks distinct allocations while retained,
original allocation identity after collection, submission identifiers and exact
completion counts. The queue's unit tests cover concurrent, out-of-order,
duplicate and unknown completions; compile contracts reject use of either frame
type after submission.

With Advanced headers and a runtime [selected for the tests](#repeating-the-tests),
measure submission with 8, 512 and 4096 completed but uncollected frames:

```bash
GRAFTON_NDI_REQUIRE_RUNTIME=advanced cargo test --locked --release --config ci/offline.toml \
  --lib --features advanced benchmark_queued_submission_backlog -- --ignored --nocapture
```

The benchmark reuses collected frames for 10,000 submit/collect cycles at each
backlog size, with no receiver. Results include SDK work and completion waits.
To isolate wrapper costs, select the test-owned `fake_objects,fake_advanced`
runtime with `GRAFTON_FAKE_CALLBACK_MODE=immediate` and logging disabled instead.

### Discovery Server objects

The Discovery Server advertisers and listeners need NDI 6.4.0 because of a
defect of the 6.3.2 runtime, not only because the vendor's change notes list a
fix. Measured on Linux x86_64 with the SDK's own Discovery Server on loopback,
destroying any of the four objects on 6.3.2 can abort the process with "pure
virtual method called", raised on an SDK thread while the destroy call is in
progress:
- it happens with every object type whenever the object has a live server
  connection;
- it never happens without a server;
- destroying an object soon after creating it is enough.

The 6.4.0.2 runtime does not abort in the same cases.

None of these workarounds makes it safe:
- removing registrations first;
- waiting for a settled connection;
- serializing every destroy;
- destroying on a dedicated thread;
- delaying the destroy;
- keeping objects for reuse instead of destroying them. This one fails
  differently: an exit with live objects can itself crash 6.3.2.

Never destroying them leaks about three threads and four descriptors per
object. The crate therefore refuses the four objects before 6.4.0.

Four defects remain in 6.4.0.2. The crate cannot prevent them; the
documentation of the affected methods says how to stay clear of them.
- **Listener crash on a quick move.** A listener on the server crashes when
  the same sender or receiver is removed from one advertiser and added to
  another on the same server within about 20 ms.
  - With the listener in a second process, it crashes in 10 of 12 runs on
    6.4.0.2 and 4 of 6 on 6.3.2.
  - Moves 200 ms or 1 s apart do not crash it, nor does dropping the first
    advertiser before the second registration.
- **Receiver connect blocked by an unsubscribe.** `NDIlib_recv_connect` can
  block for good in a receiver registered with a receiver advertiser when a
  listener unsubscribes from that receiver's events while it connects. The
  listener can be in the same process or another.
  - It happens with 6.3.2 and 6.4.0.2.
  - Dropping the listeners without unsubscribing does not block a connect.
  - A listener of this crate unsubscribes only when asked to.
- **Rare send-advertiser destroy hang.** `NDIlib_send_advertiser_destroy`
  can block for good, waiting on one of the advertiser's own threads: once in
  about 1.1 million destroys on 6.4.0.2 under heavy load.
- **A late listener misses earlier registrations.** An advertiser or listener
  that finds no server tries again every 10 s, with no backoff. A listener
  that connects only on such a later attempt lists only what is registered
  after it connected, never what reached the server before.
  - The listener's first attempt sends its query and a second message that
    the server answers with its registry; a retry sends only the query.
  - Sender and receiver listeners behave alike, on Standard and Advanced
    6.4.0.2.
  - Such a listener lists a registration made after it connected within 2 ms.
  - A listener created once the server is serving lists the earlier
    registrations at once.
  - `tests/discovery_server.rs` checks the two halves that work.

### Source selection and recording

`Finder::find_source` uses the existing configured finder and copies each SDK
snapshot before running the caller's predicate. Deterministic fake-runtime tests
in `tests/finder_selection.rs` cover staggered and reversed discovery, duplicate
names at different endpoints, predicate reentry, full-window host result sets,
and cache isolation between configured finders. Existing snapshot and accumulated
result budget tests also cover the public selection/cache paths. The gated fake
runtime in `tests/finder_deadlines.rs` holds a native snapshot open while other
lookups make zero, bounded, and already-expired attempts: they finish within their
own lock-wait budgets without another native fetch. Enumeration and host-cache
misses return observations accumulated before expiry. Releasing the gate permits
later selection; a native source-list wait by itself holds no snapshot mutex.
All paths retain the same bounded descriptor-copy kernel.

On Linux x86_64 with Advanced 6.4.0.2 and its File library, under the loopback-only
runner, `tests/file_source_selection.rs` selects a feed through a finder with a
unique nondefault group and a loopback IP hint. Converting that named, addressed
source to `SourceSpec` preserves the native URL field alone, as required by
`Processing.NDI.Lib.File.Recorder.h`'s name precedence rule. Recording and probing
the finalized file confirms the selected 640×360 feed rather than the available
320×180 distractor.

This single-test executable installs a private `NDI_CONFIG_DIR` before loading
NDI, with loopback IP queries and `Public` send/receive defaults. On macOS only,
it retains the offline runner's loopback discovery setting to suppress sender
mDNS. Linux and Windows leave the Discovery Server unset, avoiding the
[known Windows failure with an unreachable server](troubleshooting.md#receiving-and-sending).
It also uses a private temporary directory for native state. Its native runtime
runs apart from the concurrent File unit tests: running a second SDK process beside those
tests produced source-discovery timeouts, even with a private child configuration.
No system configuration or CI network policy is changed.

The test requires deliberate name-only recording of a separate default-group
480×270 source. Both recordings must contain video and probe with the expected
distinct dimensions. An unavailable `127.0.0.1:9` endpoint paired with that proven
reachable public name must record no video, demonstrating no hidden name fallback.
The shared prerequisite helper reports missing runtimes/File libraries and applies
`GRAFTON_NDI_REQUIRE_RUNTIME`; once those exist, every native assertion is required.
SDK-independent recorder-boundary and async-command tests also verify the native
name/URL fields and endpoint copies after caller inputs are dropped.

Reproduce with `NDI_RUNTIME_DIR_V6` selecting the 6.4 Advanced runtime and
`GRAFTON_NDI_REQUIRE_RUNTIME=file`:

```sh
cargo test --config ci/offline.toml --features file-io --test file_source_selection -- --nocapture
```

Tests of `FileRecorder::reset` check that it returns the old session's report
without claiming that the files were finalized, that late events of the old
session do not reach later ones, and that the original recorder stays usable
when its replacement cannot be created.

Two File API functions are deliberately not called:
`NDIlib_file_recorder_get_source` and `NDIlib_file_recorder_get_current_file`.
They return borrowed strings that the SDK can invalidate on its own recording
thread while it stops or rolls over, so copying them at once does not make them
safe to read, and a Rust mutex around the calls cannot exclude the SDK's thread.
The Rust API provides `last_accepted_source` and `reported_current_file` from
its own state and the recorder's events instead, and documents how they differ
from an instantaneous native query. These exceptions can go when the vendor
provides a stable lifetime or a copying variant.

`ci/sdk.py` exports the expected version, edition and exact library path of the
package it provisions, and the tests compare them with the runtime loaded in
their own process. `tests/runtime_identity.rs` checks that a mismatch of each
fails the run, so knowing which archive was unpacked is not enough to pass.

## Retained SDK packages

[`ci/sdk-packages.json`](../ci/sdk-packages.json) pins the packages the tests
use: Standard 6.1.1 for Linux, and Standard and Advanced 6.3.2 and 6.4.0.2 for
Linux, Windows, Apple and Android. The CI matrix runs Standard 6.1.1 on Linux
and Standard and Advanced 6.3.2 on Linux, Windows and macOS as baselines, and
crosses the oldest retained Standard and Advanced packages with the current
Advanced one in both directions: Standard 6.1.1 on Linux, 6.3.2 elsewhere.

The Linux 6.1.1 installer comes from RPM Fusion's source cache and matches the
SHA-512 in its
[historical source manifest](https://github.com/rpmfusion/ndi-sdk/blob/d0147e01a716c8e27d03894d79c296054e8caf63/sources).
Its `Version.txt` reads `NDI 2024-12-20 r143744 v6.1.1`. The unmodified vendor
installer has its own SHA-256 pin in `ci/sdk-packages.json`.

The crate is written for every NDI 6 runtime, and its capability checks keep a
runtime from being asked for what it lacks.

## Platform coverage

| Platform | Coverage |
| --- | --- |
| Linux x86-64 | Test suite against Standard 6.1.1, 6.3.2 and 6.4.0.2 and Advanced 6.3.2 and 6.4.0.2 |
| Linux x86, Linux arm64 | Test suite against Standard 6.4.0.2 |
| macOS arm64 | Test suite against Standard and Advanced 6.3.2 and 6.4.0.2 |
| macOS x86-64 | Test suite against Standard 6.4.0.2 |
| Windows x64 | Test suite against Standard and Advanced 6.3.2 and 6.4.0.2 |
| Windows x86 | Test suite against Advanced 6.4.0.2 |
| 32-bit ARM Linux, Android | Linted with clippy, all features and warnings denied; nothing runs |
| iOS, tvOS, visionOS | Library built against the NDI SDK for Apple 6.4; nothing runs |

The test suites run in the `sdk` job of `.github/workflows/rust.yml`, on
GitHub's hosted runners; the [CI guide](../ci/README.md) describes its rows and
what each one checks. Tests leave out what a platform's runtime cannot do, and
print that they did; the
[known issues](troubleshooting.md) list those limits.

Not covered:

- NDI 6 releases other than 6.1.1, 6.3.2 and 6.4.0.2.
- Hardware-dependent behavior: PTZ devices, KVM devices, hardware codecs, and
  interoperability of compressed streams with other products.
- License entitlements. A capability reported as available says only that the
  runtime exports the functions and has the required version.

## Repeating the tests

The commands below are for Linux x86_64. Replace the paths with those of your
installation. The fake-runtime tests need `rustc` and `readelf` on the `PATH`.

Without any NDI installation:

```bash
cargo test --locked --all-features
```

With a Standard SDK installed:

```bash
export NDI_RUNTIME_DIR_V6="/path/to/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --locked --config ci/offline.toml --all-features
```

With an Advanced SDK installed:

```bash
export NDI_SDK_DIR="/path/to/NDI Advanced SDK for Linux"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=advanced,file,discovery-server cargo test --locked --config ci/offline.toml --all-features
```

`--config ci/offline.toml` starts every test executable with loopback
networking only and a private temporary directory, which on Linux needs
`bubblewrap`. The tests that use a real runtime create NDI senders, receivers
and finders. Without that option they announce themselves on the local
network. The [CI guide](../ci/README.md#loopback-only-test-runs) describes the
runner and why the tests need a private temporary directory.

[`ci/sdk.py`](../ci/README.md#sdk-packages) installs an SDK package into a
directory of your choice and prints the variables above, together with the
`GRAFTON_NDI_EXPECTED_*` values that make the tests check the runtime they
load. The crossed runs set `NDI_SDK_DIR` to one SDK, `NDI_RUNTIME_DIR_V6` and
the expected values to another, and the `GRAFTON_NDI_PEER_*` values to the SDK
that is not loaded. The
[CI workflow](../.github/workflows/rust.yml) shows each step.

To check that the crate still compiles for a target you cannot run:

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy --all-features --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

iOS, tvOS and visionOS targets need the NDI SDK for Apple at build time:

```bash
export NDI_SDK_DIR="/path/to/NDI SDK for Apple"
cargo check --all-features --target aarch64-apple-ios
```

The static checks, and the check that the generated bindings match an SDK, are
listed in [CONTRIBUTING.md](../CONTRIBUTING.md#checks-before-review).

## When the supported versions change

When an SDK version or platform is added, add it to
[What each runtime provides](#what-each-runtime-provides), with the runtime's
own version text (`ndi.info().raw_version()`), and to
[Platform coverage](#platform-coverage), saying whether the test suite runs
there or the target is only compiled.

## Image color conversion

### What the SDK does

Measured on Linux x86_64 with the 6.4.0.2 Standard and Advanced runtimes,
sender and receivers in one process on loopback, where the transport carried
flat content exactly:

| Question | Finding |
| --- | --- |
| Matrix used to send RGB as YUV | BT.709, limited range, at 720×576, 1920×1080 and 3840×2160: black became Y 16, white Y 235 |
| Matrix used to deliver YUV as RGB | BT.709 at every size tried, from 720×576 to 3840×2160, across both of the manual's boundaries |
| Does `ndi_color_info` change either conversion? | No: frames tagged BT.601 or BT.2020, alone or inside `ndi_metadata_group`, converted alike |
| Does the tag reach receivers? | Yes, unchanged, on UYVY, P216 and RGBX receivers |
| Chroma upsampling to RGB | Repeated: chroma changes only between the two pixels a sample covers and the next |
| 4:2:0 to 4:2:2 | Repeated: each NV12 chroma row became two identical rows |
| Precision of the SDK's RGB | A fixed-point approximation: white comes out 253, saturated blues fall up to 10 codes short, and green alternates by one code between even and odd pixels |

The NDI manual's table, BT.601 for SD, BT.709 for HD and BT.2020 for UHD,
describes neither conversion the 6.4 SDK performs, so the encoder does not
follow it. `tests/image_color_sdk.rs` pins these findings against a real
runtime.

What follows:

- For an untagged frame, matrix and chroma follow the SDK, so a snapshot of a
  UYVY frame matches what an RGB receiver of the same source shows, within the
  SDK's approximation: at most 10 code values, 1.5 on average.
- The coefficients are exact, so white is 255.
- A tagged frame follows its tag, although the SDK's RGB conversion ignores
  it: a source tagged BT.2020 encodes correctly here, and with BT.709
  coefficients in an RGB receiver.
- An untagged BT.601 source, such as older SD hardware, needs
  `ColorPolicy::Override`.

### The conversion

Readers hand the color stage 16-bit codes (an 8-bit sample shifted left by 8,
which keeps limited-range codes exact), so one kernel serves every depth. With
n the source's bits a sample and s = 2^(16 − n):

- limited range: Y' = (Y − 4096) / 56064, P = (C − 32768) / 57344;
- full range: Y' = Y / ((2^n − 1)·s), P = (C − 2^15) / ((2^n − 1)·s);
- R = Y' + 2(1 − K<sub>R</sub>)·P<sub>R</sub>, B = Y' + 2(1 − K<sub>B</sub>)·P<sub>B</sub>,
  G = (Y' − K<sub>R</sub>·R − K<sub>B</sub>·B) / (1 − K<sub>R</sub> − K<sub>B</sub>).

| Matrix | K<sub>R</sub> | K<sub>B</sub> |
| --- | --- | --- |
| BT.601 | 0.299 | 0.114 |
| BT.709 | 0.2126 | 0.0722 |
| BT.2020 | 0.2627 | 0.0593 |

R, G and B are clamped to 0 to 1 and rounded to the image's depth. The kernel
works in fixed point, `i32` for 8-bit images and `i64` for 16-bit ones, and its
tests hold it within one code of an `f64` reference at both depths.
