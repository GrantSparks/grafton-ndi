# Maintain the NDI wrapper

## Where changes belong

| Files | Responsibility |
| --- | --- |
| `src/runtime.rs`, `src/capabilities.rs` | Runtime selection, initialization by reference count, capability queries |
| `src/sys/` | The private native boundary: generated bindings, typed function tables, the loader that fills them |
| `src/finder.rs` | Source discovery, owned descriptors, address parsing, and lookup cache |
| `src/receiver.rs` | Receiver configuration, capture views, reconnect synchronization, status, and controls |
| `src/capture.rs` | Private SDK capture/free strategies and guards that keep buffers tied to their owners |
| `src/frames.rs` | Owned and borrowed frames, layout/metadata validation, channel access, and the `video_accessors!` macro that gives every video frame type its accessors and image encoders |
| `src/image/` | Image encoding (`image-encoding`): options, the five row readers, the color kernel, reduction, and the PNG, JPEG and WebP codecs, one file each |
| `src/color_info.rs` | The `ndi_color_info` metadata scanner and `ColorInfo`, with no feature |
| `src/framesync.rs` | Clock-corrected pull capture and receiver ownership boundaries |
| `src/sender.rs` | Synchronous sends, owned async video pipeline, copied submissions, and completion handlers |
| `src/asynchronous.rs` | Tokio/smol blocking adapters, deadlines, task errors, and cancellation |
| `src/routing.rs`, `src/discovery/` | Routing sources, Discovery Server advertisers and listeners |
| `src/advanced/` | Optional Advanced SDK configuration, packets, queues, allocators, synchronization and KVM |
| `src/file/` | Optional File API recording and playback |
| `src/rejected.rs`, `src/error.rs` | `Rejected<T>`, the error that hands a consumed value back, and the crate's `Error` |
| `src/audio_conversion.rs`, `src/video_conversion.rs` | Checked SDK audio/video conversions |
| `src/media.rs`, `src/time.rs` | Shared media values (`FrameRate`, `FourCc`, `StreamTier`, `MediaTypes`) and time values (`Timecode`, `SdkTimestamp`) |
| `src/sys/advanced.rs`, `src/sys/advanced_tables.rs` | The Advanced SDK's function tables, grouped into families, and the one place that depends on which families the installed headers declare |
| `build.rs`, `build_support.rs`, `build_apple.rs`, `build_advanced.rs` | Advanced declarations for the `advanced` feature and the selection of the families they declare, static linking for Apple mobile targets |
| `xtask/` | The generator for the checked-in bindings |
| `.github/workflows/`, `ci/`, `scripts/` | The CI and release workflows and their scripts; see the [CI guide](../ci/README.md) |
| `src/test_support.rs`, `tests/common/`, `tests/support/` | The skip policy and runtime-identity checks of the tests, shared helpers, and the fake native runtimes the loader tests compile |
| `tests/sdk_peers.rs` | A sender and a receiver of two different SDK versions, in separate processes, exchanging media |
| `examples/`, `tests/` | Application workflows, SDK regressions, and public compile contracts |
| `docs/`, `CHANGELOG.md` | The guides, the migration guides, the record of SDK validation, and release notes |

Keep shared behavior in its existing implementation. For example, every video
frame type encodes images through one `image::Source` view of its validated
parts; receiver and FrameSync views share validated frame layouts. Fix layout arithmetic once rather than adding
separate receive, send, and FrameSync calculations.

`CaptureKind` exposes public frame-type associations and owned conversion.
SDK routing and free strategies are private. New generic APIs should not expose
raw handles or SDK structures merely to reuse an internal implementation.

## Native functions and bindings

Nothing outside `src/sys` converts an address into a function pointer or looks a
symbol up by name. Every native call goes through a table defined with the
`native_table!` macro:

- The macro checks each signature at compile time against the generated
  declaration of the same name, so a table cannot drift from the SDK header.
- A table is resolved once, when the runtime is selected, and it resolves
  completely or not at all. A wrapper that holds a table can call every function
  in it.
- `Core` in `src/sys/api.rs` holds the functions every supported NDI 6 runtime
  exports. A library that lacks one of them is rejected when it is loaded. Every
  other table is one optional capability.

### Adding a native function

1. Find its declaration in `src/sys/declarations.rs`. If it is not there, the
   bindings predate the SDK version that added it; see
   [Regenerating the bindings](#regenerating-the-bindings).
2. Add it to a table in `src/sys/api.rs`. Put it in an existing table only when
   it belongs to that table's capability and every runtime that exports the
   table's other functions also exports this one. Otherwise add an entry to the
   `optional_tables!` invocation. One entry states the table, the field that
   holds it and the `RuntimeFeature` it is gated and reported under, followed
   by its functions:

   ```text
   /// Source routing.
   struct Routing in routing: Routing {
       fn NDIlib_routing_create(p_create_settings: *const NDIlib_routing_create_t) -> NDIlib_routing_instance_t;
       ...
   }
   ```

   Do not add to `Core` unless every supported runtime exports the function.
3. A new capability is one entry in the `runtime_features!` list in
   `src/capabilities.rs`: its documentation, its variant, whether it is
   `core` or `advanced` and the name it is reported under, then any of:

   - `since` and a minimum version, only when the exported functions are not
     evidence enough;
   - `requires` and the capabilities whose tables its operations also take,
     each listed before it. A configured router, for example, goes on to use
     the functions of `Routing`, so `AdvancedRouterConfiguration requires
     Routing`. Once the capability's own gate passes, it reports the status
     of the first requirement that is unavailable, and its operations are
     refused under the capability itself, with that requirement's reason and
     evidence, before they take any other table;
   - `ungated` and the reason, for a core capability that every runtime
     provides.

   A core capability is gated by its table in `optional_tables!`, by `since`
   or by `requires`. The crate does not compile when a core capability has
   none of these and does not say `ungated`, or says it and has one.

   The enum, its name, version, requirement and edition lookups, the place
   `Capabilities` keeps its status and the tests' list of every capability
   are generated from that entry.
4. Call the function from a wrapper that obtained the table with
   `ndi.table::<sys::Routing>()?`. That accessor is the only way to reach an
   optional table, and it applies the capability's gate. `src/routing.rs`
   shows the pattern. The wrapper keeps the `Ndi` handle for as long as its
   native object exists.

A test fails when a declared function is unclassified:
`every_declared_function_is_in_one_table_or_explicitly_unwrapped` in
`src/sys/coverage_tests.rs`. A function the crate deliberately does not call
goes into an `UNWRAPPED` list with the reason: the one in that file for the
core SDK, the one in `src/sys/file.rs` for the File library. The File library's
functions are classified only in a build with `file-io`, so run the test with
`--all-features`.

Functions of the File library go in an entry of the `file_families!`
invocation in `src/sys/file.rs`, which names the table, the field that holds it
and the `FileFamily` it is gated and reported under. `FileRuntime::table`,
which every player, recorder and probe goes through, is the only way to a
family's table, and refuses one the library lacks with
`UnsupportedCapability`. A new family also needs a `FileFamily` variant.

Functions that only the Advanced SDK headers declare go in the
`advanced_tables!` invocation in `src/sys/advanced_tables.rs`. Those headers may
not be redistributed. The invocation holds only the name and signature of each
function the crate calls; the headers' own declarations, types and constants
are generated into `OUT_DIR` on machines that have an Advanced SDK, are never
checked in, and are what every signature is checked against when the crate is
compiled. Each entry of the
invocation is a family of functions that are used together, and names, as an
`optional_tables!` entry does, the `RuntimeFeature` it is gated and reported
under; a new family needs its own `advanced` entry in the `runtime_features!`
list in `src/capabilities.rs`. The build script
reads the same file, through `build_advanced.rs`, and makes a family available
only when the installed headers declare every one of its functions, so an older
Advanced SDK that lacks a family still builds the same public API. Code that
calls a family is written inside `native!(Family, table => ...)`, which compiles
the body only when that family is declared. The Advanced coverage test in
`src/sys/advanced_coverage_tests.rs`, which has its own `UNWRAPPED` list, runs
only in a build with Advanced headers. Lint and test Advanced changes with
`NDI_SDK_DIR` naming an Advanced SDK, and check them against the oldest
retained Advanced SDK as well as the newest. `tests/advanced_headers.rs` builds
the crate against synthetic partial headers to keep older SDKs building; it
needs `libclang` and runs only in a build with Advanced headers.

### Regenerating the bindings

`src/sys/bindings.rs` (types and constants) and `src/sys/declarations.rs`
(functions) are generated. Never edit them by hand. Regenerate them when
adopting a newer SDK:

```bash
cargo xtask bindgen --sdk "/path/to/NDI SDK for Linux"
```

The generator needs `libclang` and `rustfmt`, and an SDK whose headers include
the File API header (NDI 6.4 or later) and whose root contains its
`Version.txt`. It reads only headers that carry the
MIT permission notice, and it generates for eleven targets and requires the
output to be the same for all of them, so one pair of files serves every
platform. The files are written already formatted, so `cargo fmt` and `rustfmt`
leave them unchanged. The first lines of each file name the SDK version they
came from.

To verify that the checked-in files match an SDK without changing them:

```bash
cargo xtask bindgen --check --sdk "/path/to/NDI SDK for Linux"
```

Run the check after regenerating and whenever `xtask/` changes. After a
regeneration, the coverage test names every new function that needs a table or
an `UNWRAPPED` entry.

## Safety and SDK contracts

For changes touching FFI, cite the applicable installed SDK header or
[NDI SDK documentation](https://docs.ndi.video/all/developing-with-ndi/sdk) in the
PR and explain the specific contract the code relies on. Put the lasting safety
argument beside the unsafe operation. State which allocation owns each pointer,
how its length and alignment are established, and which synchronization or free
operation ends SDK access. Reference header names and summarize the contract
without copying vendor source or documentation into the repository.

The important project invariants are:

- A live SDK resource keeps an `Ndi` handle, which keeps the runtime
  initialized. Normal `Ndi` acquisition and destruction follow reference
  counting. Keeping the library loaded is separate from keeping NDI
  initialized; permanent initialization requires the explicit
  `ProcessLifetimeRuntime` opt-in.
- Every call uses the one library selected for the process. A wrapper takes its
  table before it creates a native object, so a create function is never called
  without its matching destroy and free functions.
- What a runtime exports, what its version implies, its edition, and hardware or
  license availability are separate facts. Sending a frame checks its format against the selected runtime; a Cargo
  feature must not bypass this check.
- The `advanced` API has the same shape with and without the Advanced SDK
  headers. An operation that cannot run reports `UnsupportedCapability`.
- Frame layout fields stay private. Checked dimensions, plane sizes, strides,
  and metadata bounds must remain valid after every safe mutation. Follow the
  [SDK frame layouts](https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types),
  including alpha planes and the zero-length metadata sentinel.
- Borrowed video exposes initialized active rows, excluding SDK row padding.
  Use plane/row views for padded layouts; video `data()` can return `None`.
  SDK-to-owned video copies pack active planes tightly using the shared layout.
- Borrowed audio exposes initialized samples, not inter-channel padding. Use
  channel slices for strided buffers; `data()` can return `None`.
- Async video submissions retain owned pixels and metadata until the SDK's
  [send synchronization boundary](https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send)
  has completed. `send_video_async_copy` joins the same owned-frame pipeline.
  Memory safety must not depend on a caller running a destructor: safe Rust can
  forget values. The ordinary `Sender` pipeline has no completion token. The
  opt-in Advanced queue has separate exclusive ownership and native completion
  rules; it must not change ordinary send semantics.
- Completion handlers run after state is consistent and outside internal locks.
  Callback panic or reentry must not free an in-flight buffer or notify twice.
- Borrowed receive views remain tied to their receiver or FrameSync owner.
  Receiver frame references permit scoped thread transfer; FrameSync frame
  references remain thread restricted. Immutable `VideoPlane`, `VideoRows`, and
  `VideoPlanes` support scoped transfer/shared reads from either family without
  SDK calls. Their borrows retain the frame, whose guard alone frees its storage. Do not broaden auto traits without the SDK contract and a proof
  covering both access and freeing. Preserve existing owned auto traits, including
  `UnwindSafe`, when changing internal ownership; test them explicitly.
- Discovery snapshots keep the SDK buffer protected until descriptor strings
  have been copied. Reconnect/capture locking must preserve the original timeout
  budget, including zero-timeout behavior.
- Dropping an async future only suppresses work before its cancellation gate.
  A task that passed the gate can still enter SDK code after the future is gone;
  an SDK call cannot be interrupted by dropping the future.
- An operation that takes ownership of a caller's object returns that object
  when it fails.

Do not test invalid pointers by dereferencing them. Construct malformed
*descriptors* over allocations that satisfy the tested unsafe preconditions, or
assert that validation rejects a descriptor before any access occurs.

## Tests that demonstrate the change

Run the relevant existing tests while developing, and add a regression that
would fail for the original behavior. Useful evidence depends on the change:

- Layout changes: exact-size and undersized buffers, padded rows/channels,
  planar offsets, alignment, and overflow boundaries.
- Metadata changes: null and empty strings, explicit lengths and zero-length
  scanning, terminator boundaries, invalid UTF-8, and owned-copy lifetimes.
- Sending changes: original allocation identity through recovery, immediate
  source reuse after copied submission, mixed send modes, callback panic/reentry,
  and destruction with an outstanding frame. SDK tests without a receiver can
  complete immediately, so pointer/ownership assertions matter too.
- Capture changes: owner lifetimes, thread restrictions, FrameSync/raw-capture
  boundaries, and matching SDK free operations.
- Async changes: blocking-pool queue delay, reconnect-lock contention,
  zero-timeout calls, cancellation before and after work starts, and task panics.
- Loader and capability changes: the search order, a library that is rejected,
  a second selection, missing optional functions, and both sides of a version
  gate.
- Build changes: a missing or invalid `NDI_SDK_DIR` in the builds that read
  it, and static archive selection for Apple mobile targets.
- Callback changes: immediate/delayed, concurrent, out-of-order and duplicate
  completion, allocator replacement/reset, late free, panic containment and budgets.

### Tests that need native code

A test that needs a real NDI runtime is not marked `#[ignore]`. It asks for the
runtime first and returns when there is none:

```rust
#[test]
fn sender_reports_its_source() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    // The test body runs only where a runtime could be loaded.
    let options = grafton_ndi::SenderOptions::new("test sender");
    let sender = grafton_ndi::Sender::new(&ndi, &options).unwrap();
    assert!(!sender.source().unwrap().name().is_empty());
}
```

The helpers live in `src/test_support.rs`. Unit tests use them as
`crate::test_support`, and integration tests through `mod common;`:

| Helper | Reports a skip, and returns `None` or `false`, when |
| --- | --- |
| `ndi()` | no NDI runtime library can be loaded |
| `advanced_ndi()` | the runtime is not an Advanced SDK runtime, or the build lacks the declarations for Advanced sender configuration |
| `hx_decoder(&ndi, decoder)` | the runtime is known to have no NDI\|HX video decoder; `decoder` is `Requirement::H264Decoder` or `Requirement::Hevc444Decoder`, the codec the test sends. On Linux the helper loads `libndihx.so`, `libavcodec-ndi.so.61` or `libavcodec.so.61` from beside the runtime library or by name through the dynamic loader, which is where NDI 6.4.0.2 looks. On Windows and macOS nothing shows it beforehand, so the helper answers `true` there. A test that receives NDI\|HX video therefore also passes each frame's size to `is_hx_placeholder`, and calls `skip(decoder, …)` when the runtime delivered nothing but its black 1920×1080 placeholder |
| `skip(Requirement::…, reason)` | called directly by a test that found another requirement unmet, such as the File library or the Discovery Server |

A skip is printed to standard error as `skipped: <test> needs <requirement>`,
with the reason on first use. `#[ignore]` is kept for manual benchmarks and slow
stress tests only. A test that needs a fixture creates it. `unique_name(label)`
appends the process id to a sender or router name, so that two test processes
on one machine do not collide on it (see below).

A test that connects a receiver to a sender of its own process addresses the
sender through `common::loopback_source(&ndi, &sender)`, or
`common::loopback_endpoint(&ndi, name)` for a source it knows only by name.
Both find the sender's port with a finder and name `127.0.0.1:<port>`.
When a receiver does not connect, `common::socket_table(port)` adds the operating
system's view of that port to the failure message on Windows.

`GRAFTON_NDI_REQUIRE_RUNTIME` turns skips into failures. Its value is a
comma-separated list of what the environment provides: `1` for any runtime,
`advanced`, `file`, `discovery-server`, and `h264-decoder` and
`hevc-444-decoder` for an NDI|HX decoder of H.264 and of HEVC 4:4:4. An
environment that sets
`GRAFTON_NDI_REQUIRE_RUNTIME=advanced` fails every test that would have skipped
for lack of an Advanced runtime, so a run that was meant to cover the Advanced
paths cannot pass without them. An entry that is not one of the six is
rejected when a skip consults it. An entry that no test in the build can check
fails the run as well, for example `advanced` or `h264-decoder` without the
`advanced` feature, because such a promise could never be broken.

Two more groups of variables make a run check what it ran against:

- `GRAFTON_NDI_EXPECTED_RUNTIME`, `GRAFTON_NDI_EXPECTED_VERSION` and
  `GRAFTON_NDI_EXPECTED_EDITION`. When they are set, `ndi()` asserts that the
  loaded runtime is that file, that four-component version and that edition
  (`standard` or `advanced`). `ci/sdk.py` prints them. A value left over from
  another runtime fails every test that loads one.
- `GRAFTON_NDI_PEER_RUNTIME`, `GRAFTON_NDI_PEER_VERSION` and
  `GRAFTON_NDI_PEER_EDITION`: the exact library path, version and edition of a
  second SDK, which must be a different version. With them set,
  `tests/sdk_peers.rs` starts a second process on that runtime, which sends
  video, audio and metadata to a receiver on the selected runtime; a missing
  peer or a delivery failure fails the test. Without `GRAFTON_NDI_PEER_RUNTIME`
  the test prints its own skip, which `GRAFTON_NDI_REQUIRE_RUNTIME` does not
  turn into a failure.

The NDI runtime reserves every sender and receiver name machine-wide with a
lock file in `TMPDIR`, named `NTK_SEND_<sender name>` or `NTK_RECV_<receiver
name>`. Two test runs that share a temporary directory can therefore fail each
other. Give each run its own `TMPDIR`, or, on Linux and macOS, use the
loopback-only runner described under
[CI isolation](../ci/README.md#loopback-only-test-runs), which does.

The loader, identity, ownership, Discovery Server and File-library scenarios
in `tests/runtime_loading.rs`, `tests/runtime_shadowing.rs`,
`tests/runtime_identity.rs`, `tests/extended_lifetimes.rs`,
`tests/discovery.rs` and `tests/file_loading.rs` need no SDK. They are
Linux-only: they compile a fake NDI runtime from `tests/support/fake_runtime.rs`
with `rustc` when they run, and `tests/runtime_loading.rs` also calls
`readelf`. `tests/file_loading.rs` is compiled only with `file-io`. Add a
variant to the fake runtime when a loader or capability change needs a library
that behaves in a particular way.

### Checks before review

For a focused Rust change, the usual starting checks are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-features
```

Before requesting review of changes spanning public APIs or feature gates, run
the feature combinations separately. A feature that compiles only together with
another one is a defect:

```bash
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo clippy --all-targets --no-default-features --features advanced -- -D warnings
cargo clippy --all-targets --no-default-features --features file-io -- -D warnings
cargo build --examples --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test --no-default-features
cargo test -p xtask
```

A build without the Advanced SDK headers compiles none of the `advanced`
module's native code and relaxes the unused-code lints for it. Changes under
`src/advanced/`, to `src/sys/advanced.rs` or `src/sys/advanced_tables.rs`, or
to `build_advanced.rs` must therefore also be linted and tested with the
headers present, with the newest Advanced SDK and with the oldest retained one,
whose headers declare fewer families:

```bash
export NDI_SDK_DIR="/path/to/NDI Advanced SDK for Linux"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
cargo clippy --workspace --all-targets --all-features -- -D warnings
GRAFTON_NDI_REQUIRE_RUNTIME=advanced cargo test --all-features
```

The PNG and thumbnail examples are skipped without `image-encoding`, and the
thumbnail example also without `file-io`. `advanced` and `file-io` are
separate opt-ins. Features never select the runtime edition.

For dependency, build, or language-feature changes, check library compatibility
separately from contributor tooling:

```bash
cargo +1.88.0 check --workspace --all-features --all-targets
cargo audit --deny warnings
```

`Cargo.lock` is tracked. Use `--locked` to build exactly what it records, and
commit it when a dependency changes. Keep the bindgen versions of the build
script and of `xtask` in step. Keep base64 at 0.22 until its SIMD engine has
been validated separately.

The crate must keep compiling for targets you cannot run. Checking another
target needs only its Rust standard library:

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy --all-features --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

Report the SDK version, platform and checks actually exercised. Say which
platforms or SDK editions were not covered rather than implying an untested
combination passed.

## Compile contracts

`tests/compile_contracts.rs` checks the public examples in `tests/ui/pass` and
checks expected failures in `tests/ui/fail`. They cover owned async transfer,
source reuse after copied sends, receive lifetimes, thread restrictions, and
private capture machinery. Advanced fixtures cover exclusive owner and scoped
clock restrictions. Run against a consistent source tree: an unrelated dependency
build failure invalidates a compile-contract result, even if a harness reports
that the individual case compiled.
Pass fixtures that reference SDK resources keep that code in uncalled contract
functions with an empty `main`, so trybuild checks types without starting NDI.

```bash
cargo test --all-features --test compile_contracts --test advanced_contracts
```

Use `TRYBUILD=overwrite cargo test --test compile_contracts` only when the
contract intentionally changes or a stable compiler changes diagnostic wording.
Review every changed `.stderr`: the fixture must still fail for the intended
ownership, lifetime, visibility, or auto-trait reason. Do not accept a snapshot
that passes merely because an unrelated missing method now causes an error.
For an unexpected success, check the fixture directly before changing an expected
error. Preserve the intended error code and ownership reason when compiler wording
changes. The `rust-src` toolchain component keeps standard-library source
diagnostics consistent between local and CI runs. Public API changes also need
semver and auto-trait checks against the previous released API.

## SDK upgrades and platform qualification

To adopt a newer NDI SDK:

1. Regenerate the bindings from its headers and run the check, as described
   under [Regenerating the bindings](#regenerating-the-bindings).
2. Run the tests. The coverage test lists each newly declared function; give
   it a table or an `UNWRAPPED` entry.
3. Read the SDK's change notes for behavior changes in functions the crate
   already calls. A capability whose functions exist in older runtimes but
   misbehave there needs a `since` version in its `runtime_features!` entry.
4. Pin the new packages in `ci/sdk-packages.json`, and update the `sdk` matrix
   in `.github/workflows/rust.yml` and its table in the
   [CI guide](../ci/README.md#the-sdk-job); the script tests check that the two
   agree. The private source map needs an entry for every package the matrix
   uses.
5. Run the full test suite against the new runtime and against the oldest
   retained runtime of each edition (Standard 6.1.1 and Advanced 6.3.2 on
   Linux), with the headers and runtimes crossed and with the peer test.
6. Update the version and platform tables in the
   [setup guide](setup.md#supported-ndi-versions-and-platforms), stating for each
   platform whether the tests run against a real runtime or the crate is only
   compiled and linted, and list each new SDK defect under its
   [known issues](troubleshooting.md).

The crate supports NDI 6 runtimes. A passing symbol or layout check does not
prove callback lifetimes, codec interoperability or hardware behavior.

Never commit SDK archives, SDK headers, generated declarations of the Advanced
SDK headers, or credentials.
