# Grafton NDI 3.0.0 code security review — scratchpad

Date: 2026-11-13
Scope: Rust library code and its native SDK boundary. GitHub repository issues, settings, and release administration are out of scope.
Status: Review findings for maintainer analysis; no source fixes made in this review.

## Executive summary

The most concrete concern is the borrowed video-frame send path: on x86-64, bytes after a UYVY frame can affect the received picture. The repository's SDK integration test demonstrates this, and the public unsafe builder currently says those bytes need not be initialized. The Finder snapshot path also lacks an aggregate limit on SDK-supplied source counts and copied data. Exact file-recording names have a time-of-check/time-of-use gap whose exploitability depends on native SDK file-opening behavior. The default runtime-library search order is a deployment-sensitive design choice to review where executable directories are writable by an attacker.

| Item | Priority for 3.0.0 | Confidence | Status |
| --- | --- | --- | --- |
| Trailing borrowed video bytes affect output | High | High for output influence; conditional for sensitive-data disclosure | Reproduced with installed Standard SDK |
| Finder snapshot has no aggregate resource budget | Medium | High for missing bound; remote triggerability unproven | Source review |
| Exact recorder filename race | Medium if recording in shared directories | High for race; overwrite outcome unverified | Source review; File SDK unavailable |
| Default library search trusts executable directory | Deployment dependent | High for search order; exploitation depends on filesystem control | Design consideration |

## Threat model and review method

The relevant boundaries are application-provided frame buffers, network-discovered source descriptions surfaced by the NDI SDK, native callbacks and frames returned by the SDK, recorder output paths, and the runtime library loaded into the process. A malicious or defective native runtime can violate FFI pointer contracts; this review identifies places where the wrapper can cheaply enforce resource limits but cannot promise safety against an arbitrary hostile native library.

I inspected frame layout and metadata validation, pixel buffer handling, send staging, Finder and Discovery Server snapshots, advanced compressed-packet validation, allocator and queued-send callbacks, file-recording paths, asynchronous file-command admission, and dynamic runtime loading. I ran the available unit and integration tests described below. This was not a fuzzing campaign, a full native SDK reverse engineering effort, or a live network adversary test.

## Finding 1: Borrowed video trailing bytes can influence encoded pixels

**Evidence.** `PixelFormat::send_tail_room()` returns 64 bytes for UYVY and UYVA on the tested x86-64 target (`src/frames.rs:395`). `BorrowedVideoFrameBuilder::build()` counts last-row padding, bytes in the supplied slice beyond the frame layout, and the separately asserted `readable_after` region as tail room (`src/sender.rs:1232-1259`). When that count satisfies the SDK's requirement, `sends_in_place()` avoids the zeroed staging copy (`src/sender.rs:1021-1075`). The `readable_after` safety text says the region may be read by the SDK and compressed into the picture, yet says it “need not be initialized” (`src/sender.rs:1177-1218`). Owned pixel buffers explicitly zero their spare tail (`src/pixels.rs:163-167`). The existing integration test changes trailing bytes and checks that the received picture changes on x86-64; it also shows that an exact-length slice takes the zeroed staging path (`tests/send_tail_room_sdk.rs:236-317`).

**Reproduction.** On this workspace's installed Standard NDI runtime, the following completed successfully, with the runtime required so a missing SDK could not silently skip it:

```sh
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo +1.100.0 test --locked --test send_tail_room_sdk bytes_after_a_buffer_reach_the_picture_only_as_room -- --nocapture
```

**Security consequence.** A caller may place non-pixel, potentially sensitive bytes in the same initialized slice immediately after a borrowed frame or in its final-row padding. Those bytes can influence received image blocks. The unsafe `readable_after` method additionally permits an asserted region that is readable but uninitialized, allowing the SDK to read bytes whose contents the caller has not established. The test proves output influence, not recovery of arbitrary trailing bytes or a reliable secret-exfiltration primitive. The impact is limited to affected pixel formats, target/runtime behavior, and calls that use in-place borrowed sending with nonzero tail room.

**Recommended change.** Tighten the unsafe contract to require every byte the SDK may read to be initialized and stable for the send duration. For confidentiality by default, stage UYVY/UYVA borrowed frames into a zero-tailed buffer unless the caller explicitly opts into a trusted, initialized tail. Preserve an in-place fast path for callers that can provide deliberately zeroed padding. Extend the SDK test to pin the default behavior selected for 3.0.0.

## Finding 2: Finder source snapshots lack an aggregate resource limit

**Evidence.** `Finder::snapshot()` takes an SDK-written `u32` source count and a pointer, calls `Vec::with_capacity(count as usize)`, then visits every descriptor with pointer arithmetic (`src/finder.rs:242-269`). `Finder::current_sources()` uses that helper (`src/finder.rs:352-365`), as does the Advanced source-with-metadata path (`src/advanced/finder.rs:80-102`). Individual SDK strings are bounded at 4 MiB (`src/frames.rs:2593-2616`), but the snapshot has no count cap or combined byte budget. By comparison, the Discovery Server snapshot has a 16 MiB copy budget and checked count-times-element-size logic (`src/discovery/listener.rs:487-588`).

**Security consequence.** A very large SDK-reported count can provoke a large allocation, an allocation failure that terminates the process, or prolonged descriptor and string copying. The count originates from the SDK's current discovery snapshot. Network-discovered sources plausibly contribute to that snapshot, but this review did not demonstrate that one remote peer can force a chosen count or create an arbitrarily large valid SDK array. If the SDK returns an incorrect count for its pointer, pointer validity remains an FFI contract problem; a wrapper-side cap reduces exposure but cannot validate every native pointer.

**Recommended change.** Make `snapshot()` return `Result<Vec<T>>`; reject counts beyond a documented maximum before preallocating, use `try_reserve`, and enforce a total copied-byte budget, including source names, addresses, and metadata. Have both public Finder methods propagate a clear resource-limit error. Add focused tests using an injected snapshot fetcher and valid test descriptors around the limit; avoid tests that actually allocate enormous arrays.

## Finding 3: Exact recording-name guarantee has a TOCTOU gap

**Evidence.** `NamePattern::check()` uses `symlink_metadata` to reject an exact path that already exists (`src/file/types.rs:587-607`). `FileRecorder::start()` checks the path, does other work, and then calls `NDIlib_file_recorder_start` (`src/file/recorder.rs:613-640`). `rollover()` checks and then calls `NDIlib_file_recorder_rollover` (`src/file/recorder.rs:1026-1041`). The code comments and error text state that an exact name never replaces a file, but the check and SDK open are separate operations.

**Security consequence.** If the recording directory is writable by another actor, that actor can create a file or symlink after the check. Whether an existing target is overwritten, followed, or rejected depends on the native File SDK's opening flags and path policy, which were unavailable for verification here. The race itself is certain; a practical overwrite vulnerability is unconfirmed.

**Recommended change.** Verify the File SDK's behavior with a controlled race or vendor documentation before relying on the “never replaces” guarantee. If the SDK does not provide atomic exclusive creation, narrow the API promise and require a trusted recording directory, or arrange for an atomic reservation/open protocol compatible with the SDK. Test start and rollover separately.

## Additional design consideration: runtime library search order

Without an explicit path, the loader searches a companion directory, then the executable directory, then the runtime directory, then platform loader paths (`src/sys/loader.rs:601-648`). Loading a library from an attacker-writable executable directory can execute attacker-controlled native code in the application process. This is an intentional and documented packaging behavior, not a demonstrated vulnerability in a trusted installation. For deployments that cannot guarantee directory ownership, prefer an explicit trusted runtime path or a policy that does not prioritize the executable directory. Review this as a deployment contract for 3.0.0 rather than as a standalone release blocker.

## Protective controls observed

- Video, audio, and metadata constructors enforce 256 MiB, 64 MiB, and 4 MiB limits with checked layout arithmetic (`src/frames.rs:2171-2179`, `src/frames.rs:2820-2907`, `src/frames.rs:3237-3272`).
- Advanced compressed packets enforce a 256 MiB maximum and validate lengths before packet use (`src/advanced/packets.rs:13`, `src/advanced/packets.rs:326-385`).
- Discovery Server snapshot copying uses a shared 16 MiB budget and checked descriptor sizing (`src/discovery/listener.rs:487-588`).
- Owned frame tails are zeroed before the SDK can read them (`src/pixels.rs:163-167`).
- Native callback paths inspected use panic containment and ownership tracking (`src/advanced/allocators.rs`, `src/advanced/queued.rs`).
- Advanced JSON configuration size is capped and sensitive fields are redacted from its debug representation (`src/advanced/config.rs`).

These controls narrow the audit findings; they do not amount to a proof that every FFI path is memory safe.

## Verification and limitations

| Check | Result |
| --- | --- |
| `cargo +1.99.1 test --locked --lib --all-features` | 626 passed, 2 ignored |
| `python3 -m unittest discover -s ci/tests -q` | 93 passed |
| `cargo-audit audit --file Cargo.lock --deny warnings` | Exited 0; 122 dependencies scanned; crates-index update warning appeared |
| Targeted tail-room SDK test with `GRAFTON_NDI_REQUIRE_RUNTIME=1` | Passed against installed Standard SDK |
| `cargo +1.100.0 test --locked --all-features --test advanced_contracts --quiet` | 24 passed |
| Full `cargo +1.100.0 test --locked --all-features --quiet` | Unit and most integration tests passed; one local sender discovery timeout caused suite failure |
| Targeted `receiver_status_notifications` rerun | Passed in 18.53 seconds |

The Rust 1.99.1 all-features run also encountered `trybuild` compiler-output snapshot differences; the same advanced contract suite passed with Rust 1.100.0. The full-suite discovery timeout was not reproduced in the targeted rerun and is not classified as a security finding. Standard NDI SDK 6.3.2.0 was present. Advanced and File SDK-specific runtime behavior could not be exercised here, so their findings rely on source inspection and stated SDK contracts. No source files were modified for this report. `src/file/asynchronous.rs` was already modified by another workspace actor during the review; that worktree change was not included as a finding or altered here.

## Suggested next analysis

1. Decide whether 3.0.0 should default borrowed UYVY/UYVA sending to zeroed staging, then update the unsafe tail contract and integration test together.
2. Choose Finder count and aggregate byte limits, accounting for realistic large deployments, and add fallible allocation before release.
3. Verify File SDK exact-name creation semantics with an SDK-enabled test environment; update the guarantee or implementation based on the result.
4. Document the trusted-directory requirement for default runtime loading, or expose an application policy that selects only an explicit trusted runtime.
