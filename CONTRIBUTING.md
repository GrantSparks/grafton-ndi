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
version is 1.88; development dependencies and compiler-diagnostic fixtures use
current stable. Required CI checks Rust 1.88 on representative Linux x64 SDK
versions and the baseline Android ARM64 SDK, and uses nightly for formatting.
Additional Apple targets use the toolchain specified by their profile; that does not raise the crate-wide MSRV. Run results,
not workflow definitions, establish validation.

Start with:

```bash
cargo build
cargo test --lib
cargo run --example NDIlib_Find
```

The discovery example needs a working NDI runtime and network environment.
Ordinary `cargo test` skips the 39 registered SDK-dependent core tests; their
runtime evidence comes from the confined runner described below. Run manual
device tests that advertise or receive synthetic sources on an appropriate
isolated test network. The [development SDK store](ci/sdk-store.md) provides
verified side-by-side installations. Keep vendor archives, generated bindings and
SDK inspection output outside the source checkout.

## Where changes belong

| Files | Responsibility |
| --- | --- |
| `src/runtime.rs`, `src/dispatch.rs`, `src/capabilities.rs` | Runtime selection, one retained native provider, initialization leases, typed dispatch and capability evidence |
| `src/finder.rs` | Source discovery, owned descriptors, address parsing, and lookup cache |
| `src/receiver.rs` | Receiver configuration, capture views, reconnect synchronization, status, and controls |
| `src/capture.rs`, `src/recv_guard.rs` | Private SDK capture/free strategies and guards that keep buffers tied to their owners |
| `src/frames.rs` | Owned and borrowed frames, layout/metadata validation, channel access, and image encoding |
| `src/framesync.rs` | Clock-corrected pull capture and receiver ownership boundaries |
| `src/sender.rs` | Synchronous sends, owned async video pipeline, copied submissions, and completion handlers |
| `src/async_runtime.rs` | Tokio/smol blocking adapters, deadlines, task errors, and cancellation |
| `src/routing.rs`, `src/discovery/` | Routing, retained advertisements, listener snapshots and events |
| `src/advanced/` | Optional configuration, packets, queues, allocators, synchronization and KVM |
| `src/file/` | Optional thread-affine File owners, owned events, finalization and async commands |
| `src/utilities.rs`, `src/video_conversion.rs`, `src/timestamp.rs` | Checked SDK audio/video conversions and distinct SDK timestamps |
| `src/ndi_lib.rs` | Private generated ABI declarations and existing-call dispatch forwarding |
| `build.rs`, `build_support.rs`, `build_apple.rs`, `build_profiles.tsv` | SDK discovery, target/artifact selection, ABI checks and static export verification |
| `api/`, `tools/sdk/` | Reviewed SDK coverage/capability metadata and independent target ABI probes |
| `ci/` | Hosted validation policy, verified SDK provisioning and development store |
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
operation ends SDK access. Reference header names and summarize the contract
without copying restricted vendor source into public review material. Generated
Advanced declarations stay in `OUT_DIR`; curated ABI declarations need recorded
redistribution provenance and retained notices.

The important project invariants are:

- A live SDK resource retains its native provider and initialization lease.
  Normal `NDI` acquisition/destruction follows the production reference-counting
  state machine. Shared module pinning is separate from keeping NDI initialized;
  permanent initialization requires the explicit `ProcessLifetimeRuntime` opt-in.
- Existing and optional calls use the same selected core module. Validate complete
  create/destroy/free groups before creating a resource. Never reinterpret a short
  older SDK function table as a newer layout. Audit actual executable imports;
  unused generated extern declarations alone are not dynamic imports.
- Installed headers, compiled ABI, resolved symbols, version-gated enum semantics,
  edition evidence and hardware/license availability are separate facts. New frame
  formats retain their runtime proof; a Cargo feature must not bypass this check.
  Older headers still expose stable Rust API shapes with explicit unavailability
  where a native ABI cannot be compiled.
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
- Build changes: target/artifact selection, actual static exports, rejected ABI
  mismatches, missing optional libraries and mixed header/runtime versions. A Rust
  loader build does not prove the vendor C link interface; test both where required.
- Callback changes: immediate/delayed, concurrent, out-of-order and duplicate
  completion, allocator replacement/reset, late free, panic containment and budgets.
- File changes: source/path ownership, finalization evidence, timeout retention,
  automatic stop/rollover, event loss and owner-thread destruction. Do not replace
  the two unavailable recorder pointer getters with an unproven borrowed slice.
- SDK upgrades: reviewed declaration/export changes, independent production-ABI
  comparisons, same-major enum gates and generated capability-registry drift.

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
cargo test --examples
cargo test --examples --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

These ordinary Cargo commands run pure tests, fake-SDK loader contracts and
runnable doctests. They do not replace the opt-in native core, regression,
Advanced or File suites. CI points ordinary tests' default runtime resolver at a
fresh empty private directory, preventing accidental default-SDK loads; explicit
fake-module fixtures still choose their own paths. The example test command also
exercises the PCM conversion regressions in `NDIlib_Recv_Audio_16bpp.rs`.

For optional-feature changes, also exercise the independent configurations.
Building examples explicitly is important: library tests alone do not establish
that application workflows still compile.

```bash
cargo test --no-default-features
cargo build --examples --no-default-features
cargo test --examples --no-default-features
cargo test --no-default-features --features tokio
cargo build --examples --no-default-features --features tokio
cargo test --examples --no-default-features --features tokio
cargo test --no-default-features --features smol
cargo build --examples --no-default-features --features smol
cargo test --examples --no-default-features --features smol
cargo test --no-default-features --features tokio,smol
cargo build --examples --no-default-features --features tokio,smol
cargo test --examples --no-default-features --features tokio,smol
cargo check --no-default-features --features advanced_sdk
```

The PNG example is skipped without `image-encoding`. `advanced_sdk` remains a
no-op; `advanced` and `file-io` are separate opt-ins. Check their builds both with
Standard headers and with the authorized SDK supplying the native API. An
all-feature Standard build exercises unavailable-ABI behavior; it does not prove
Advanced or File runtime operation. Features never select the runtime edition.

The confined runner executes all 39 core tests individually on native and
compatibility checks, and repeats them for each of the nine feature selections
in feature checks. Add SDK-dependent tests to [the core registry](ci/core_tests.py)
and mark them with its ignore reason. A native test must require a successful
runtime initialization rather than treating a missing SDK as a passing test.

For example, with a Standard Linux x64 SDK and working unprivileged `bwrap`
network namespaces:

```bash
export NDI_SDK_DIR='/path/to/NDI SDK for Linux'
export NDI_SDK_PROFILE='x86_64-linux-gnu'
export NDI_SDK_EDITION='standard'
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
export NDI_EXPECTED_RUNTIME="$NDI_RUNTIME_DIR_V6/libndi.so"
export CARGO_TARGET_DIR='/path/to/private/target'
cargo generate-lockfile
python ci/native.py --mode core --feature-set all --target x86_64-unknown-linux-gnu --output /path/to/private/core-results
python ci/native.py --mode regressions --target x86_64-unknown-linux-gnu --output /path/to/private/regression-results
```

Use fresh output directories. `--feature-set 0` through `8` selects the exact
ordered configurations in [FEATURE_SETS](ci/common.py), with default features
disabled; `all` selects all features. Windows requires elevated PowerShell and
Windows Defender Firewall control. The macOS executor is restricted to disposable
GitHub-hosted runners; do not invoke it on a development Mac. Native raw logs stay
private. See the [CI guide](ci/README.md) for boundary checks and evidence limits.

Run the normally ignored Advanced/File suites using their documented fixtures and
confinement, and record the selected header SDK and actual loaded runtime separately:
[Advanced guide](docs/advanced.md), [File guide](docs/file-io.md), and
[validation policy](ci/README.md).

For sender lifetime/synchronization changes, run the confined `regressions` mode;
it includes the owned async pipeline and repeated sender lifecycle tests.

For dependency, build, or language-feature changes, check library compatibility
separately from contributor tooling:

```bash
cargo +1.88.0 check --all-targets --all-features
cargo +1.88.0 test --lib --all-features
cargo generate-lockfile
cargo audit --deny warnings
```

The library does not track `Cargo.lock`. Audit the freshly resolved graph across
all optional and target dependencies. Keep bindgen at 0.72.1 until mixed
`syn`/`prettyplease` consumer resolutions are validated; retain base64 0.22 until explicit SIMD-engine validation. See
[the 2.0 migration guide](docs/migration/1.x-to-2.0.md#async-std-to-smol) for
application lockfile updates.

Report the SDK/platform and checks actually exercised. Identify unavailable
platform or Advanced SDK coverage rather than implying an untested matrix passed.

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

## SDK upgrades and platform qualification

The active provisioning and validation path is documented in [ci/README.md](ci/README.md)
and [ci/sdk-store.md](ci/sdk-store.md). The catalog contains 12 reviewed bundles:
four Standard 6.3.2 platforms and eight Standard/Advanced 6.4 platforms. Review and
retain SDKs as they arrive. The compatibility target is NDI 6.x; NDI 6.0 and 6.1.1
do not have current validation results.

Use [tools/sdk/README.md](tools/sdk/README.md) to snapshot, compare and classify
SDK changes, regenerate capability metadata and run independent ABI probes. A
new or changed declaration needs a reviewed disposition; an unavailable wrapper
must remain visibly unavailable. Keep original archive hashes, header/runtime
identity and test results together. A passed symbol or layout check does not prove
callback lifetimes, codec interoperability or hardware behavior.

The supported release matrix is limited to artifacts and toolchains that can be
acquired and exercised on GitHub-hosted runners. The full artifact inventory also
retains vendor BSP candidates outside that supported matrix. Add a candidate only
with an obtainable, reviewed toolchain and the required hosted checks; do not
substitute a generic same-CPU sysroot for a vendor ABI. Physical-device results
remain separate manual qualification and never require a private CI runner.

Required CI groups 52 checks into 16 SDK runner jobs. Full native suites use
Linux x64, Windows x64 and macOS ARM64 representatives in both SDK editions; other
supported profiles retain ABI/link or packaging checks. The broader 82-check
audit is available through the explicit `extended` scope. Add a new required
combination when it covers a distinct risk, rather than multiplying every
architecture, SDK edition, version and compiler by default.

Record the measured coverage: native, emulator/simulator, or build/link-only.
Public Debian armhf checks for the four RPi GNU profiles use a glibc 2.36 build
sysroot; they do not prove glibc 2.17 deployment or board-specific CPU behavior.
A prepared workflow or SDK inventory is not a successful hosted run. See the
[SDK validation record](docs/sdk-validation.md) for current and historical evidence.

Private source maps contain locators; authentication is a separate credential.
Never commit either private maps or credentials, SDK archives, or generated vendor
bindings. Remote release assets can be replaced or deleted: verified archive hashes
establish the expected content, not immutable remote storage. Public source review
and hosted validation do not authorize publication of SDK materials or release
promotion. The old composite setup actions remain
[documented as historical tooling](.github/actions/README.md); the active workflow
uses the shared provisioner instead.
