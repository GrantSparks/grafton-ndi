# SDK integration validation

This document states what was tested for the current version, what was not, and
how to repeat the tests on your own machine. The
[README](../README.md#supported-ndi-versions-and-platforms) summarizes it. The
crate supports NDI 6 runtimes and Rust 1.88 or later.

## What was tested for 3.0.0

The test suite was run on **Linux x86_64** with `cargo test --locked`. Every run
against a real runtime used `--config ci/offline.toml`, which confines each test
executable to loopback networking and a private temporary directory. Every such
run also set `GRAFTON_NDI_EXPECTED_RUNTIME`, `GRAFTON_NDI_EXPECTED_VERSION` and
`GRAFTON_NDI_EXPECTED_EDITION`, so each test that loaded a runtime checked that
it had loaded exactly the intended file, version and edition. The counts are the
totals of every test binary and the doctests.

### Runs with one SDK

| Advanced headers | Runtime | Features | Required to be present | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- | --- | --- |
| None | None | all | Nothing | 636 | 0 | 3 |
| None | Standard 6.1.1 | all | `1` | 636 | 0 | 3 |
| None | Standard 6.1.1 | none | `1` | 398 | 0 | 2 |
| None | Standard 6.3.2 | all | `1` | 636 | 0 | 3 |
| None | Standard 6.3.2 | `image-encoding` | `1` | 424 | 0 | 3 |
| None | Standard 6.3.2 | none | `1` | 398 | 0 | 2 |
| Advanced 6.3.2 | Advanced 6.3.2 | all | `advanced` | 639 | 0 | 3 |
| None | Advanced 6.3.2 | none | `1` | 398 | 0 | 2 |
| None | Standard 6.4.0.2 | all | `1,discovery-server` | 636 | 0 | 3 |
| None | Standard 6.4.0.2 | none | `1,discovery-server` | 398 | 0 | 2 |
| Advanced 6.4.0.2 | Advanced 6.4.0.2 | all | `advanced,file,discovery-server` | 639 | 0 | 3 |
| None | Advanced 6.4.0.2 | none | `1,discovery-server` | 398 | 0 | 2 |
| None | Advanced 6.4.0.2 | `file-io` | `1,file,discovery-server` | 508 | 0 | 2 |

"Advanced headers" is the Advanced SDK that `NDI_SDK_DIR` named for a build
with the `advanced` feature, whose Advanced-only headers that build reads. No
other build reads an SDK, so "None" also covers a build made where a Standard
SDK, or an Advanced SDK without the feature, was installed. The core bindings
are always the vendored 6.4.0.2 ones. "Required to be present" is the value of
`GRAFTON_NDI_REQUIRE_RUNTIME`: what the environment promised, so that a test
could not pass by skipping. In the first row the system's runtime was hidden
and the tests that need a runtime reported skips. The other rows visibly skip
the Advanced or File operations that their build and runtime cannot provide;
the requirements make sure that the operations a row promises were not
skipped. The ignored tests are a manual benchmark, a slow stress test and,
with all features or `image-encoding`, a second manual benchmark.

`tests/receiver_video_only.rs` used to send without a clock, so it sent video
and audio as fast as its loop turned: about 17,000 frames of each a second,
some 500 times real time. With every core busy, the runtime dropped the control
receiver's connection within the first second and had not restored it 30
seconds later, even after the load and the sending had stopped, and the test
failed. It now sends in real time, clocked 30 fps video with 1/30 s of audio per
frame. With twice as many busy processes as the machine has cores, the old test
failed 3 of 3 runs against Standard 6.4.0.2; the new one passed 10 of 10 against
each of Standard and Advanced 6.4.0.2. The rows above were run with the new
test, one at a time.

### Runs that cross versions

These are the runs the `sdk` CI job makes on its Linux x64 Advanced 6.4.0.2 row,
all with every feature enabled. In each, a second process on the "peer" runtime
sends video, audio and metadata to a receiver on the run's runtime, and
`tests/sdk_peers.rs` checks the decoded pixels, nonzero audio samples, frame
timecodes and metadata. Both processes are confined to loopback, and each checks
the identity of the runtime it loaded.

| Advanced headers | Runtime | Peer sending | Required to be present | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- | --- | --- |
| Advanced 6.4.0.2 | Standard 6.1.1 | Advanced 6.4.0.2 | `1` | 639 | 0 | 3 |
| Advanced 6.4.0.2 | Advanced 6.3.2 | Advanced 6.4.0.2 | `advanced` | 639 | 0 | 3 |
| None | Advanced 6.4.0.2 | Standard 6.1.1 | `1,file,discovery-server` | 636 | 0 | 3 |
| Advanced 6.3.2 | Advanced 6.4.0.2 | Advanced 6.3.2 | `advanced,file,discovery-server` | 639 | 0 | 3 |

So 6.1.1 and 6.4 exchanged media in both directions, as did Advanced 6.3.2 and
6.4, and Advanced builds ran against runtimes of another version in both
directions.

To require the peer test locally, set `GRAFTON_NDI_PEER_RUNTIME` to the second
SDK's exact library path, `GRAFTON_NDI_PEER_VERSION` to its four-component
version, and `GRAFTON_NDI_PEER_EDITION` to `standard` or `advanced`; the peer
must be a different version from the runtime the tests load. With these set, a
missing peer, an identity mismatch, a delivery failure or a failed shutdown of
the child process fails the test. Without them it reports a skip.

### What each runtime provides

The runtimes report their versions as `6.1.1` (for 6.1.1), `6.3.2.0` and
`6.4.0.2` at the end of their version text. A build with the Advanced SDK
6.4.0.2 headers reported these capabilities for them:

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
6.3.2 SDKs include no File library; the File library of the 6.4.0.2 SDK was
tested only beside the Advanced 6.4.0.2 runtime.

### Image encoding

The image encoding of 3.0 was added after the runs above, so their counts do
not include its tests. On 2026-11-13, on the same machine:

| Test | Runtimes | Result |
| --- | --- | --- |
| `tests/image_color_sdk.rs`, the SDK's color conversion pinned (BT.709 at 720×576, 1920×1080 and 3840×2160 whatever the tag, tags passed through, repeated chroma, RGB sent as BT.709 limited range, a PQ tag reaching an RGB receiver, FrameSync frames) | Standard 6.1.1, 6.3.2 and 6.4.0.2; Advanced 6.4.0.2 | 5 of 5 passed on each |
| `tests/video_padding_sdk.rs`, UYVY and P216 frames encoded in place from the SDK's padded rows | Standard 6.1.1, 6.3.2 and 6.4.0.2; Advanced 6.3.2 and 6.4.0.2 | 2 of 2 passed on each |
| The same under Valgrind Memcheck | Standard 6.3.2 and 6.4.0.2 | No errors in the crate's code; four in glibc's dynamic loader with 6.3.2, none with 6.4.0.2 |

An earlier Memcheck run of that test found the SDK reading past the end of the
UYVY frame it was sending, which the next section measures. See the
[image encoding guide](image-encoding.md) and its
[performance notes](image-encoding-performance.md).

### Reads past the end of sent video

The x86 builds of the SDK read past the last pixel of many UYVY and UYVA
frames they send. The SDK's documentation gives a UYVY frame as its line
stride times its height, asks for no alignment and no padding, and asks only
that widths be even; its own examples allocate exactly that. The builds for
64-bit ARM Linux read every frame exactly.

#### x86-64

The reads were measured on 2026-11-13, on the same machine, with C programs
that call only the SDK. The main one sends frames from a buffer that ends
where an inaccessible page begins, and moves the frame back from the page a
byte at a time until no read reaches it. The frames held a slow ramp, and in
the first two runs below also uniform random bytes, with the same result.

Progressive frames, by size:

| Runtimes | Formats | Sizes | Result |
| --- | --- | --- | --- |
| Standard 6.1.1, 6.3.2 and 6.4.0.2; Advanced 6.3.2 and 6.4.0.2 | UYVY | 640×480, 720×480, 720×576, 854×480, 1280×720, 1366×768, 1920×1080, 1918×1080, 1080×1920, 640×1080, 642×32, 3840×2160 | The same on every runtime. No read past 640×480, 1280×720, 1920×1080 and 3840×2160. 4 bytes past 720×480, 720×576 and 1918×1080, 12 past 640×1080, 16 past 1080×1920, 20 past 854×480 and 1366×768, 28 past 642×32 |
| The same | UYVA | The same | The same on every runtime. 5 bytes past 854×480, 7 past 720×480, 720×576 and 642×32, 8 past 1366×768, 12 past 640×1080; none past the others |
| The same | P216, PA16, NV12, I420, YV12, BGRA, BGRX, RGBA, RGBX | The same; 3840×2160 PA16 as a ramp only | No read past the frame |
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

Standard 6.1.1 and 6.3.2 and Advanced 6.4.0.2 gave the same figures at the
four of these sizes they were run at. Over 51 widths from 2 to 3840,
interlaced and single-field UYVY frames 32, 480 and 1080 rows high, 441 cases,
were read at most 28 bytes past.

A height that is not a multiple of four is read further when the frame is
interlaced or a single field. At 1920×1082, 1920×1086, 2560×1442, 3840×2162
and 26 other sizes, all larger in area than 1920×1080, an interlaced UYVY
frame was read to the end of the row after next, two line strides past the
frame, on every runtime it was run on; single fields of 2560×1083 and
3840×1083 were read one row past. No progressive frame was, no frame whose
height is a multiple of four, and no UYVA frame of those sizes. Frames of the
other formats at those sizes are converted by the SDK first, and those runs
ended in a fault inside the SDK, outside the buffers it was given.

Three more observations bear on what a program sees:

- The reads happen on every frame, whether or not a receiver is connected.
- Where a frame ends within its page matters. In 7,715 runs on the five
  runtimes, of 275 sizes with a width that is a multiple of 16 in three scan
  modes, a frame that ends exactly on a page boundary was read exactly. Under
  Valgrind Memcheck, which can mark the bytes after a frame anywhere in a
  page, a 720×480 frame that ends elsewhere was read 4 bytes past. The
  method above keeps the frame's end near a page boundary, so a size's
  figure is the furthest read seen and not a bound.
- What the SDK reads is compressed into the picture. A 642×32 UYVY frame
  followed by bytes of 16, and the same frame followed by bytes of 235,
  arrived with 12 samples different, by up to 3, all in the last two columns
  of the last eight rows.

The SDK's own `NDIlib_Send_Latency` example, built with the 6.4.0.2 SDK and
changed only in its width, shows the reads under Valgrind Memcheck: no error
at its own 640×480, one read 4 bytes past its buffer per frame at 720×480,
and reads 28 and 4 bytes past it at 642×480.

#### 64-bit ARM

The Linux builds for 64-bit ARM were run on the same machine under QEMU 7.2
user-mode emulation, with the same program. None read past a frame: Standard
6.4.0.2 over 1,743 UYVY and UYVA cases of 38 widths, six heights and the four
scan types, each at four positions, and 48 cases of six other formats;
Standard 6.1.1 and 6.3.2 and Advanced 6.4.0.2 over 300 cases each.

#### 32-bit x86

The 32-bit x86 Linux build of Standard 6.4.0.2 was not run here, where 32-bit
programs cannot be linked. On GitHub's hosted Ubuntu 24.04 runner, in run
43365028704, `tests/send_tail_room_sdk.rs` found that it reads a progressive
642×32 UYVY frame exactly, where the x86-64 builds read past it. No other size
was measured, so the crate keeps the x86 room for it.

The same runtime, in runs 43366775596 and 43369298234, returned
`tests/image_color_sdk.rs`'s 3840×2160 UYVY picture of flat 64×64 patches with
4% of its bytes changed, by up to 224 at patch edges, and with the chroma
inside some patches wrong too: the mean of Cb inside one patch of the leftmost
column was 40.8 code values off, while every patch's luma held within 2. Its
720×576 and 1920×1080 frames were within 2 inside every patch. In run
43371812571 two receivers of the same stream were not given the same
3840×2160 picture: the SDK's RGB frame for one differed from the BT.709
conversion of the other's UYVY frame by up to 255, 1.14 on average. The test
bounds that runtime's 3840×2160 chroma by what was measured, and every other
size and runtime by 2, and pins the SDK's colour conversion on that runtime
with the smaller sizes only.

#### What the crate does

`PixelFormat::send_tail_room` is the room the crate gives the SDK after a
frame: 64 zero bytes for UYVY and UYVA, none for the other formats, and none
at all on 64-bit ARM Linux. An interlaced or single-field UYVY or UYVA frame
whose height is not a multiple of four gets two line strides more. An owned
frame's allocation holds the room; `Sender::send_video` sends a borrowed
frame in place when its buffer has the room, or the caller vouches for it
with `BorrowedVideoFrameBuilder::readable_after`, and from a copy in a buffer
the sender keeps when it does not. On other architectures and operating
systems, where nothing was measured, the crate takes the x86 figures.

The copy costs time. Sending a UYVY frame from a buffer that ends with it,
against the same frame with room, took 0.15 ms longer at 1280×720, 0.28 ms at
1920×1080 and 1.1 ms at 3840×2160 with the 6.3.2 and 6.4.0.2 runtimes, in a
release build on the same machine: 18%, 15% and 54% of the send.

| Test | Runtimes | Result |
| --- | --- | --- |
| `tests/send_tail_room_sdk.rs`: UYVY and UYVA frames of nine sizes and the four scan types in memory that ends at an inaccessible page, with exactly the crate's room and with none, through every video send; and what the bytes after a frame do to the picture, as room, after a buffer and vouched for, with the SDK's reading of them pinned | Standard 6.1.1, 6.3.2 and 6.4.0.2 and Advanced 6.3.2 and 6.4.0.2 on x86-64; Standard 6.4.0.2 for 64-bit ARM under emulation | 2 of 2 passed on each |
| The same, and `tests/video_padding_sdk.rs` sending from vectors that end with the frame, under Valgrind Memcheck | Standard 6.3.2 and 6.4.0.2 on x86-64 | No read or write outside an allocation with either runtime, apart from the four reports in glibc's dynamic loader with 6.3.2. The first test also draws reports of uninitialised values, 82 with 6.3.2 and 140 with 6.4.0.2, every one of them in memory the SDK allocated; the second draws none |

With the room cut to one row, or to 24 bytes, or the copy disabled, the first
test ends in a segmentation fault on x86-64.

Programs of the same kind found nothing outside their buffers on any of the
five x86-64 runtimes in the audio sends, planar and interleaved, over 644
shapes of 1 to 16 channels and 1 to 1,920 samples, or in the V210, P216 and
audio conversion utilities, over 1,218 cases with a guard before and after
both buffers.

### Static checks

On the same machine these checks passed: formatting; clippy with warnings
denied for the workspace with all features, and for the crate with no features
and with each of `image-encoding`, `tokio`, `smol`, `advanced`, `file-io` and
`advanced,file-io`; the same `advanced` lints with the Advanced SDK 6.4.0.2
headers; rustdoc with warnings denied, with and without those headers; a
`cargo check` with Rust 1.88; `cargo package`; and the check that the vendored
bindings match the SDK 6.4.0.2 headers.

## Compatibility safeguards and additional runtime checks

Which Advanced operations a build can perform follows from the declarations
generated from the installed headers. A family of functions that the headers
lack disables only that family, and types or receive presets that older headers
lack do not stop them compiling. The build script and the runtime tables take
the families from one registry, `src/sys/advanced_tables.rs`. The coverage test
requires every Advanced function the installed headers declare to belong to a
family or to be listed as deliberately not wrapped. `tests/advanced_headers.rs`
builds the crate, with warnings denied, against synthetic partial headers and
checks the capabilities that build reports. That tests the fallback; it is not
a run against a historical vendor SDK.

Further Linux x86_64 tests against 6.4 check a real lossless HEVC 4:4:4 frame,
including every sample of its three full-resolution planes; audio suppression
by both video-only bandwidth settings, with an ordinary receiver as a control;
AVSync following changes in video size, frame rate, audio sample rate and
channel count while preserving nonzero samples; and both Genlock clocks
following a local source and falling back to paced system clocks. The KVM
boundary test checks failed release, retry on drop, touch coordinates and
clipboard ownership using stand-in functions; it does not qualify a KVM device.

A FrameSync and raw audio capture on its receiver compete for the receiver's
audio, which is why `Receiver<FrameSyncAttached>` has no audio capture. This
was measured by hand, on Linux x86_64 over loopback with Standard 6.3.2 and
6.4, and no test repeats it. A clocked sender sent 30 fps video, each frame
with 1600 sequence-coded samples of 48 kHz stereo audio, while a FrameSync
pulled 480 samples every 10 ms for 8 s. Without raw capture, every frame in the
window reached the FrameSync and it returned no silence. With raw audio capture
on the attached receiver, interleaved with the pulls or on a second thread,
each audio frame reached exactly one of the two: raw capture took 131 to 216 of
the 196 to 239 frames in the window, and the FrameSync returned silence for 65
to 90 per cent of the samples it pulled. Every metadata frame still reached the
attached receiver's own metadata capture.

`FrameSync` is `Sync`, although neither its header nor the SDK documentation
of 6.1.1, 6.3.2 or 6.4 says whether one FrameSync may be used from several
threads at once. That was measured by hand on Linux x86_64 over loopback. In
the runtimes of all three versions, video capture, audio capture and the video
free take the instance's exclusive lock, the audio free returns its buffer
through atomic operations, and the audio queue depth is read without the lock.
With a clocked sender of uniform gray 30 fps frames and audio, four threads
made every `&FrameSync` call on one FrameSync under helgrind on 6.3.2 and 6.4:
the only conflicts between two of those threads were the unlocked queue-depth
read and the atomic audio free, and a single-thread control showed none.
Sixteen threads made the same calls for 120 s on each of the three runtimes,
about 30 million video and 60 million audio captures each, and received no
torn video frame, no invalid audio sample and no error.
`tests/framesync_capture_boundary.rs` repeats a short version of the
multi-thread run against every runtime the tests use. Runtimes for other
platforms were not measured.

### Discovery Server objects

The Discovery Server advertisers and listeners need NDI 6.4.0 because of a
measured defect of the 6.3.2 runtime, not only because the vendor's change
notes list a fix. On Linux x86_64, in a private network namespace with the
SDK's own Discovery Server on loopback, a C harness created and destroyed each
of the four objects repeatedly in seven scenarios:
- with no server;
- connected;
- destroyed within 2 ms of creation;
- advertisers with registered senders or receivers, removed first or not;
- listeners with subscriptions and events flowing;
- eight threads destroying at once;
- the server killed or restarted first.

| Runtime | Destroys | Process aborted |
| --- | --- | --- |
| Standard 6.3.2 | 454,194 | 2,486 |
| Advanced 6.3.2 | 202,750 | 2,250 |
| Standard 6.4.0.2 | 847,530 | 0 |
| Advanced 6.4.0.2 | 838,487 | 0 |

Every abort on 6.3.2 was "pure virtual method called", raised on an SDK thread
while the destroy call was in progress:
- it happened with every object type and in every scenario that had a live
  server connection;
- it never happened without a server;
- destroying an object soon after creating it is enough.

None of these workarounds made it safe:
- removing registrations first;
- waiting for a settled connection;
- serializing every destroy;
- destroying on a dedicated thread;
- delaying the destroy;
- keeping objects for reuse instead of destroying them. This one fails
  differently: an exit with live objects can itself crash 6.3.2.

Never destroying them leaks about three threads and four descriptors per
object. The crate therefore refuses the four objects before 6.4.0.

Four defects remain in 6.4.0.2, observed the same way. The crate cannot
prevent them; the documentation of the affected methods says how to stay clear
of them.
- **Listener crash on a quick move.** A listener on the server crashes when
  the same sender or receiver is removed from one advertiser and added to
  another on the same server within about 20 ms.
  - With the listener in a second process, it crashed in 10 of 12 runs on
    6.4.0.2 and 4 of 6 on 6.3.2.
  - No crash with moves 200 ms or 1 s apart (about 4,800 and 970 moves), nor
    when the first advertiser was dropped before the second registration
    (17,390 moves).
- **Receiver connect blocked by an unsubscribe.** `NDIlib_recv_connect` can
  block for good in a receiver registered with a receiver advertiser when a
  listener unsubscribes from that receiver's events while it connects. The
  listener can be in the same process or another.
  - There were 15 permanent blocks in 12 campaigns across 6.3.2 and 6.4.0.2.
  - None occurred in about 15,400 connects when the listeners were dropped
    without unsubscribing.
  - A listener of this crate unsubscribes only when asked to.
- **Rare send-advertiser destroy hang.** `NDIlib_send_advertiser_destroy`
  blocked for good once, waiting on one of the advertiser's own threads, in
  about 1.1 million destroys on 6.4.0.2 under heavy load.
- **A late listener misses earlier registrations.** An advertiser or listener
  that finds no server tries again every 10 s, with no backoff (8 attempts
  traced over 75 s). A listener that connects only on such a later attempt
  lists only what is registered after it connected, never what reached the
  server before.
  - The listener's first attempt sends its query and a second message that
    the server answers with its registry; a retry sends only the query.
  - Sender and receiver listeners behaved alike, on Standard and Advanced
    6.4.0.2.
  - A listener created before the server never listed an earlier
    registration in 8 of 8 runs watched for 25 to 35 s.
  - It listed one made after it connected in 12 of 12 runs, within 2 ms.
  - A listener created once the server was serving listed the earlier one at
    once in 7 of 7 runs.
  - `tests/discovery_server.rs` checks the two halves that work.

No test repeats these measurements.

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
from an instantaneous native query. Complete coverage of the C entry points is
therefore not claimed. These exceptions can go when the vendor provides a
stable lifetime or a copying variant.

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

The Linux 6.1.1 installer was recovered from RPM Fusion's source cache and
matches the SHA-512 in its
[historical source manifest](https://github.com/rpmfusion/ndi-sdk/blob/d0147e01a716c8e27d03894d79c296054e8caf63/sources).
Its `Version.txt` reads `NDI 2024-12-20 r143744 v6.1.1`. The unmodified vendor
installer has its own SHA-256 pin in `ci/sdk-packages.json`.

No other SDK version has been tested: not NDI 6.0 or 6.2, not Advanced SDKs
before 6.3.2, and not the older versions for other platforms. The crate is
written for every NDI 6 runtime, and its capability checks keep a runtime from
being asked for what it lacks, but only the versions above are tested. The
Windows and Apple 6.3.2 packages were checked for version and archive identity,
and their headers were compiled, but nothing was run from them.

## What was compiled but not run

The crate was linted with clippy, all features, all targets and warnings
denied, for these targets with Rust 1.99.1. No test was run on them, and no NDI
library was loaded on them:

- `i686-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `armv7-unknown-linux-gnueabihf`
- `x86_64-pc-windows-msvc`, `i686-pc-windows-msvc`
- `x86_64-apple-darwin`, `aarch64-apple-darwin`
- `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`,
  `x86_64-linux-android`

The library was linted the same way for `aarch64-apple-ios` and
`x86_64-apple-ios` against the NDI SDK for Apple 6.4. That selects and checks
the static archive, but the static link was not performed and nothing was run.

`aarch64-apple-tvos`, `x86_64-apple-tvos` and `aarch64-apple-visionos` are
built in hosted CI, with the iOS targets; nothing is run on them.

## Hosted CI

The `sdk` job of `.github/workflows/rust.yml` runs the test suite against real
runtimes on GitHub's hosted runners: Linux x86-64 with Standard 6.1.1, 6.3.2
and 6.4 and Advanced 6.3.2 and 6.4; Linux x86 and arm64 with Standard 6.4;
macOS arm64 with Standard and Advanced 6.3.2 and 6.4, and macOS x86-64 with
Standard 6.4; Windows x64 with Standard and Advanced 6.3.2 and 6.4, and Windows
x86 with Advanced 6.4. The [CI guide](../ci/README.md) describes the rows and
what each one checks.

## What was not tested

- Any behavior on Windows, macOS, Android, iOS, tvOS or visionOS. This includes
  the Windows and macOS branches of the runtime search and the static linking
  of Apple mobile targets, which are covered by compilation only.
- NDI runtimes other than Standard 6.1.1 and Standard and Advanced 6.3.2 and
  6.4.0.2, all on Linux x86_64. NDI 6.0 and 6.2 were not tested.
- The File library on Windows, macOS and Android, and beside an Advanced 6.3.2
  runtime.
- Linux architectures other than x86_64.
- Hardware-dependent behavior: PTZ devices, KVM devices, hardware codecs, and
  interoperability of compressed streams with other products.
- How far the SDK reads past sent video on x86 processors without AVX2, on
  32-bit x86 and 32-bit ARM, and on Windows, macOS, Android and iOS, where
  its compressor may run other code. The crate gives those the room it
  measured for x86-64 Linux, more than twice the furthest ordinary read.
- Whether the SDK reads past the compressed frames an Advanced sender is
  given, or writes past the buffers of a custom receive allocator.
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

## Recording a validation

When you test a new SDK version or platform, add it to the tables above with
the runtime's own version text (`ndi.info().raw_version()`), the build inputs,
and what `GRAFTON_NDI_REQUIRE_RUNTIME` promised. Record what was run, not what
was configured: a platform that was only compiled belongs under "compiled but
not run".
