# Contributing to grafton-ndi

`grafton-ndi` wraps the NDI SDK's runtime, discovery, capture, and sending APIs.
A useful contribution explains the application behavior it changes and preserves
the ownership and memory-layout guarantees around the SDK calls.

## Local setup

Install NDI SDK 6, its runtime libraries, and a C toolchain with a discoverable
`libclang` shared library. See the [README prerequisites](README.md#prerequisites)
for platform paths, `NDI_SDK_DIR`, `LIBCLANG_PATH`, and runtime library discovery.
Building generates bindings from your installed SDK; the repository does not
contain a standalone replacement for that installation.

Use current stable Rust for development. [rust-toolchain.toml](rust-toolchain.toml)
selects stable with `clippy`, `rustfmt`, and `rust-src`. The library's minimum Rust
version is 1.87; development dependencies and compiler-diagnostic fixtures use
current stable. CI separately checks the library with Rust 1.87 and uses nightly
for its formatting job.

Start with:

```bash
cargo build
cargo test --lib
cargo run --example NDIlib_Find
```

The discovery example needs a working NDI runtime and network environment. Tests
that create SDK instances also need the runtime, even when no source is present.
Do not assume that a machine without a camera can skip installing the SDK.

## Where changes belong

| Files | Responsibility |
| --- | --- |
| `src/runtime.rs` | Process-global SDK initialization, reference counting, and `NDI` handles |
| `src/finder.rs` | Source discovery, owned descriptors, address parsing, and lookup cache |
| `src/receiver.rs` | Receiver configuration, capture views, reconnect synchronization, status, and controls |
| `src/capture.rs`, `src/recv_guard.rs` | Private SDK capture/free strategies and guards that keep buffers tied to their owners |
| `src/frames.rs` | Owned and borrowed frames, layout/metadata validation, channel access, and image encoding |
| `src/framesync.rs` | Clock-corrected pull capture and receiver ownership boundaries |
| `src/sender.rs` | Synchronous sends, owned async video pipeline, copied submissions, and completion handlers |
| `src/async_runtime.rs` | Tokio/async-std blocking adapters, deadlines, task errors, and cancellation |
| `src/ndi_lib.rs` | Private include wrapper for bindings generated into Cargo's `OUT_DIR` |
| `build.rs`, `build_support.rs` | Binding generation, SDK discovery, target ABI checks, and library selection |
| `examples/`, `tests/` | Application workflows, SDK regressions, and public compile contracts |

Keep shared behavior in its existing implementation. For example, owned and
borrowed image encoding share `ImagePixelSource`; receiver and FrameSync views
share validated frame layouts. Fix layout arithmetic once rather than adding
separate receive, send, and FrameSync calculations.

`CaptureKind` exposes public frame-type associations and owned conversion.
SDK routing and free strategies are private. New generic APIs should not expose
raw handles or SDK structures merely to reuse an internal implementation.

## Safety and SDK contracts

For changes touching FFI, cite the applicable installed SDK header or
[NDI SDK documentation](https://docs.ndi.video/all/developing-with-ndi/sdk) in the
PR and explain the specific contract the code relies on. Put the lasting safety
argument beside the unsafe operation. State which allocation owns each pointer,
how its length and alignment are established, and which synchronization or free
operation ends SDK access.

The important project invariants are:

- A live SDK resource retains a live `NDI` handle. Initialization and destruction
  must follow the production reference-counting state machine.
- Frame layout fields stay private. Checked dimensions, plane sizes, strides,
  and metadata bounds must remain valid after every safe mutation. Follow the
  [SDK frame layouts](https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types),
  including alpha planes and the zero-length metadata sentinel.
- Borrowed audio exposes initialized samples, not inter-channel padding. Use
  channel slices for strided buffers; `data()` can return `None`.
- Async video submissions retain owned pixels and metadata until the SDK's
  [send synchronization boundary](https://docs.ndi.video/all/developing-with-ndi/sdk/ndi-send)
  has completed. `send_video_async_copy` joins the same owned-frame pipeline.
  Memory safety must not depend on a caller running a destructor: safe Rust can
  forget values. There is no completion token in the current API.
- Completion handlers run after state is consistent and outside internal locks.
  Callback panic or reentry must not free an in-flight buffer or notify twice.
- Borrowed receive views remain tied to their receiver or FrameSync owner.
  Receiver views permit scoped thread transfer; FrameSync views remain thread
  restricted. Do not broaden auto traits without the SDK contract and a proof
  covering both access and freeing.
- Discovery snapshots keep the SDK buffer protected until descriptor strings
  have been copied. Reconnect/capture locking must preserve the original timeout
  budget, including zero-timeout behavior.
- Dropping an async future only suppresses work before its cancellation gate.
  A task that passed the gate can still enter SDK code after the future is gone;
  an SDK call cannot be interrupted by dropping the future.

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
- Build changes: target selection and artifact discovery tests in
  `tests/build_configuration.rs`, including unsupported ABIs and cross-builds.

For a focused Rust change, the usual starting checks are:

```bash
cargo test --lib
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

CI's formatting job uses `cargo +nightly fmt --all -- --check`. Use that command
when reproducing a formatting difference; the ordinary local command follows
the checked-in stable toolchain.

Before requesting review of changes spanning public APIs or feature gates, run
the broader checks used by [CI](.github/workflows/rust.yml):

```bash
cargo test --all-features
cargo build --examples
cargo build --examples --all-features
cargo test --examples --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

The example test command also exercises the PCM conversion regressions in
`NDIlib_Recv_Audio_16bpp.rs`.

For optional-feature changes, also exercise the independent configurations.
Building examples explicitly is important: library tests alone do not establish
that application workflows still compile.

```bash
cargo test --no-default-features
cargo build --examples --no-default-features
cargo test --no-default-features --features tokio
cargo build --examples --no-default-features --features tokio
cargo test --no-default-features --features async-std
cargo build --examples --no-default-features --features async-std
cargo check --no-default-features --features advanced_sdk
```

The PNG example is skipped without `image-encoding`. `advanced_sdk` is a retained
compatibility flag; selecting it does not enable SDK completion callbacks or
choose the installed SDK library. Passing `--all-features` against the standard
SDK does not establish coverage of an actual Advanced SDK installation.

For sender lifetime/synchronization changes, run the normally ignored SDK suites:

```bash
cargo test --all-features --test async_send_lifecycle --test sender_lifecycle_stress -- --ignored
```

For dependency, build, or language-feature changes, check library compatibility
separately from contributor tooling:

```bash
cargo +1.87.0 check --all-features
```

Report the SDK/platform and checks actually exercised. Identify unavailable
platform or Advanced SDK coverage rather than implying an untested matrix passed.

## Compile contracts

`tests/compile_contracts.rs` runs the public examples in `tests/ui/pass` and
checks expected failures in `tests/ui/fail`. They cover owned async transfer,
source reuse after copied sends, receive lifetimes, thread restrictions, and
private capture machinery.

```bash
cargo test --test compile_contracts
```

Use `TRYBUILD=overwrite cargo test --test compile_contracts` only when the
contract intentionally changes or a stable compiler changes diagnostic wording.
Review every changed `.stderr`: the fixture must still fail for the intended
ownership, lifetime, visibility, or auto-trait reason. Do not accept a snapshot
that passes merely because an unrelated missing method now causes an error.
The `rust-src` toolchain component keeps standard-library source diagnostics
consistent between local and CI runs.

## Pull requests and documentation

Describe the concrete trigger, previous behavior, resulting behavior, and why
the change belongs in the wrapper. Include test results and any relevant SDK
contract. For public API changes, explain compatibility and allocation/blocking
costs, update rustdoc and affected examples, and add an appropriate entry to
[CHANGELOG.md](CHANGELOG.md). Update migration guidance when existing callers
must change their code. Prefer shared helpers over parallel implementations.

Use a concise title describing the resulting change; existing commits commonly
use `fix:`, `feat:`, `docs:`, or a scoped form such as `fix(sender):`.
Contributions use the repository's [Apache-2.0 license](LICENSE).

## SDK installer maintenance

The platform setup actions and their configuration are documented in
[.github/actions/README.md](.github/actions/README.md). Changes to CI installers
should include the source URL, verified SHA256, and affected platform checks.
Installer payloads are checked against allowlists; cache keys include the primary
accepted hash. Changing a download URL alone neither refreshes an existing cache
nor allows an unrecognized payload. These are maintainer configuration changes,
not a prerequisite for ordinary contributions.
