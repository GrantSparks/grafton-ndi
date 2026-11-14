# Maintain the NDI wrapper

## Where changes belong

| Area | Responsibility |
| --- | --- |
| `runtime`, `capabilities`, `sys` | Runtime lifetime, capability gates and the private native boundary |
| `finder`, `receiver`, `sender`, `framesync` | Discovery, capture, sending and clock-corrected capture |
| `capture`, `frames`, `pixels`, `sdk_copy` | Buffer ownership, validated layouts and copying |
| `media`, `time`, `color_info` | Shared values and metadata interpretation |
| `asynchronous`, `advanced`, `file`, `image` | Optional adapters and feature implementations |
| `audio_conversion`, `video_conversion` | Checked SDK conversions |
| `routing`, `discovery` | Routing and Discovery Server objects |
| `error`, `rejected` | Errors and recovery of consumed inputs |
| `build*.rs`, `xtask/` | Build-time declarations, Apple linking and checked-in bindings |
| `src/test_support/`, `tests/common/`, `tests/support/` | Prerequisite policy, runtime identity and fixture helpers |
| `tests/`, `examples/` | Regressions, compile contracts and runnable workflows |
| `ci/`, `.github/workflows/` | Provisioning, checks and release operations |

Keep shared validation and ownership in existing helpers. Fix layout arithmetic
in the shared layout implementation; do not add a separate calculation for each
capture, conversion or sender path. Keep raw handles and SDK routing/free
strategies private.

Long-lived adapters own their configured dependencies: `SourceCache` owns its
finder and `FrameSync` its receiver. Borrowed capture, frame, clock and control
views express temporary access to their owners. Preserve those constraints in
compile contracts. Deferred async work must own everything it needs after an
immediate input copy.

## Native functions and bindings

Native entry points belong in `src/sys`. `native_table!` checks signatures
against generated declarations and resolves each table completely or reports
a missing function. `Core` contains required entry points; optional tables
are obtained through capability-checked accessors.

### Adding a native function

1. Find its generated declaration in `src/sys/declarations.rs`; regenerate
   bindings when adopting a newer SDK.
2. Add a standard SDK function to the appropriate table in `src/sys/api.rs`.
   Use `optional_tables!` for a new capability. Add to `Core` only when every
   supported runtime must export the function.
3. Declare a new capability once in `runtime_features!` in
   `src/capabilities.rs`. Use `since` for a version gate and `requires` for
   dependencies on other capabilities. A core capability needs a table,
   version/dependency gate, or an explicit `ungated` reason.
4. Obtain optional tables through `ndi.table::<Table>()?` and retain an `Ndi`
   handle for the native object's lifetime. Use `src/routing.rs` as a model.
5. Run declaration coverage tests. Deliberately unwrapped declarations need
   a reason in the relevant `UNWRAPPED` list.

The File library uses `file_families!` and `FileRuntime::table` in
`src/sys/file.rs`. Advanced-only functions use the shared registry in
`src/sys/advanced_tables.rs`; `build_advanced.rs` enables a family only when
its installed headers declare all of its functions. Generated Advanced
bindings stay in `OUT_DIR`.

Inside Advanced wrappers, use `native!(Family, table => ...)` for native
expressions, `declared!` for whole definitions, and the corresponding
`ndi_has_advanced_<field>` cfg for native-only fields/helpers. Keep setup and
result handling inside the selected body. Family-specific selection preserves
the public API when older headers lack an operation. Handwritten helpers must
not acquire blanket `unused` or `dead_code` allowances: the generated ABI
imports have narrow allowances for partial headers.

### Regenerating the bindings

`src/sys/bindings.rs` and `src/sys/declarations.rs` are checked in so default
builds do not need SDK headers. Generate them with `xtask` rather than editing
them by hand:

```sh
cargo xtask bindgen --sdk "/path/to/NDI SDK for Linux"
cargo xtask bindgen --check --sdk "/path/to/NDI SDK for Linux"
```

The generator needs libclang, rustfmt, both Standard and File umbrella headers,
and the SDK's `Version.txt`. It selects headers carrying the permission notice
it checks for, formats the result and checks equivalent declarations across its
configured targets. Run `--check` after regeneration or generator changes.
Keep the build script's and xtask's bindgen versions aligned.

Keep checked-in bindings, `Cargo.lock`, expected `.stderr` outputs and the tiny
codec fixtures in `tests/data/`. They support SDK-free builds, reproducible
inputs and compile contracts. `tests/data/generate.py` documents fixture
regeneration; ordinary tests consume the checked-in bytes.

## Safety and SDK contracts

For FFI changes, cite the installed header and applicable SDK contract in the
PR. Explain beside each unsafe operation who owns the allocation, its valid
length and alignment, synchronization, and the event that ends native access.
Do not copy vendor headers or manuals into the checkout.

Preserve these invariants:

- Native resources retain an `Ndi` handle. Keeping the selected library
  loaded and keeping NDI initialized are separate lifetimes; permanent
  initialization is an explicit `ProcessLifetimeRuntime` choice.
- Obtain a complete function table before creating its native object, so its
  destroy/free operations come from the same selected implementation.
- Cargo features, build declarations, runtime exports/version/edition and
  hardware or license availability are separate facts. Do not use a feature
  flag to bypass format or capability validation.
- Safe mutation preserves validated dimensions, strides, plane sizes and
  metadata bounds. Borrowed video/audio expose initialized rows/samples rather
  than native padding. Use plane/channel views where contiguous `data()` is
  unavailable; owned video copies use the shared layout.
- Owned async video, including its attached metadata, remains alive through SDK synchronization.
  Memory safety cannot depend on the caller running a destructor. Advanced
  queue ownership must preserve the ordinary sender's separate semantics.
- Restore consistent state and release internal locks before user completion
  handlers. Panic, reentry or duplicate notification must not free an in-flight
  buffer or complete it twice.
- Receive views retain their owner borrow and matching free strategy. Preserve
  receiver/FrameSync thread restrictions and owned auto traits when changing
  storage. An auto-trait change needs an SDK contract and access/free proof.
- Protect discovery snapshots until descriptor strings have been copied.
  Preserve the original timeout budget through reconnect/capture locking,
  including zero-timeout calls.
- Dropping an async future prevents work only before its cancellation gate;
  work that passed the gate can still call the SDK. Dropping a future cannot
  interrupt an SDK call already running.
- Operations that consume caller resources return those resources on failure
  through their recovery error.

Test malformed descriptors only over allocations satisfying the tested unsafe
preconditions, or reject them before access. Never manufacture an invalid
pointer and dereference it to test validation.

## Tests that demonstrate the change

Prefer a regression that exercises the public behavior and fails without the
fix. Choose the relevant boundaries:

| Change | Cases to cover |
| --- | --- |
| Layout or metadata | Exact/short buffers, padding, overflow, alignment, null/empty/terminated strings and owned-copy lifetime |
| Sending or callbacks | Input recovery, copied-source reuse, mixed send modes, outstanding destruction, panic/reentry and duplicate completion |
| Capture | Owner lifetime, thread restrictions, FrameSync/raw boundaries and matching frees |
| Async | Queue/lock delay, zero timeout, cancellation before/after work starts and task failure |
| Loading or capabilities | Rejected libraries, conflicting selection, missing functions and both sides of version gates |
| Builds | Missing/invalid SDK roots, partial Advanced headers and Apple archive selection |

Native tests use the shared helpers rather than silently returning on arbitrary
errors. `common::ndi()` skips only a missing runtime library; failed runtime
initialization panics. `GRAFTON_NDI_REQUIRE_RUNTIME` makes a missing promised
prerequisite fail. Runtime identity expectations can also fail a run. See
[CI local checks](../ci/README.md#running-the-checks-locally) for those variables,
provisioning and isolation.

Integration tests select helper groups through `common_helpers!` in
`tests/common/mod.rs`. For a sender/receiver pair in one process, use its
loopback helper and unique-name helper. Add fake-library behavior under
`tests/support/` for loader and capability regressions that need no vendor
runtime; these fixture libraries compile with warnings denied.

### Checks before review

Run the [standard checks](../ci/README.md#running-the-checks-locally), then extend
coverage according to the change:

- Public API or feature gates: build/lint/test the affected features separately,
  including default, `advanced`, `file-io`, async adapters and `image-encoding`.
  Build runnable examples and rustdoc with warnings denied.
- Advanced native code: lint/test with both newest and oldest retained Advanced
  headers. Builds without those headers omit native declaration families;
  they do not relax handwritten unused-code lints. The synthetic-header test
  in `tests/advanced_headers.rs` covers partial families when headers are present.
- Dependencies/build/language features: check the library's minimum Rust version
  separately from stable contributor tooling, review the lockfile and run the
  relevant cross-target jobs. Apple mobile targets need SDK archives; see
  [build requirements](setup.md#build-requirements).
- Public defaults, errors, capability gates, ownership or cancellation: verify
  rustdoc and workflow guides against the changed source and regression tests.
  Validate examples and local links. Describe observed native behavior only
  with the [qualification evidence](sdk-validation.md) needed to reproduce it.

Report what actually ran and what skipped. Configured matrix coverage does not
establish a successful native run.

## Compile contracts

`tests/compile_contracts.rs` and `tests/advanced_contracts.rs` use trybuild for
passing fixtures and expected compiler failures, including ownership, lifetimes,
visibility and auto traits:

```sh
cargo test --locked --all-features --test compile_contracts --test advanced_contracts
```

Pass fixtures keep native operations in uncalled functions and reference their
function items from `main`, so type checking needs no NDI runtime. Preserve
that pattern and instantiate generic contracts for the covered types.

Use `TRYBUILD=overwrite` only for an intentional contract change or changed
stable diagnostics. Review every `.stderr` difference: it must still fail for
the intended contract, with no unrelated missing method or dependency failure
masking it. Check unexpected successes directly. Public API changes also need
the repository's semver checks; see [CI](../ci/README.md).

## SDK upgrades and platform qualification

Regenerate and check bindings, classify new declarations, review SDK changes
for necessary capability gates, and update package pins and configured jobs in
[CI](../ci/README.md#sdk-packages). Keep older compatibility rows. Exercise the
new runtime and retained versions, including crossed headers/runtimes and peer
transport where configured. Update [setup](setup.md#supported-ndi-versions-and-platforms)
when support policy changes and publish [qualification evidence](sdk-validation.md)
for observed results. Symbol/layout checks alone do not prove callback lifetime,
codec interoperability or hardware behavior.

Keep SDK archives, headers, Advanced generated declarations and credentials
outside version control.
