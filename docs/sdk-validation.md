# SDK integration validation

This page maps current wrapper safeguards to their tests and defines the evidence
needed for native-runtime qualification. Test code and workflow configuration
show what is intended to be checked; they do not establish that a particular SDK
build or platform passed. Use [setup](setup.md#platforms) for deployment choices
and the [CI matrix](../ci/README.md#the-sdk-job) for configured runtime coverage.

## What the tests cover

| Scope | Assertions and source |
| --- | --- |
| Runtime loading | Selection, lifetime, path trust and expected version/edition/path; `tests/runtime_loading.rs`, `tests/runtime_lifecycle.rs`, `tests/loader_trust.rs`, `tests/runtime_identity.rs` |
| Ownership and boundary validation | Compile contracts, malformed native descriptors, release behavior and fake-runtime lifecycle checks; `tests/compile_contracts.rs`, `tests/advanced_contracts.rs`, `tests/mixed_capture_boundary.rs`, `tests/extended_lifetimes.rs` |
| Source selection | Configured finders, descriptor budgets, lock-wait deadlines and cache isolation; `tests/finder_selection.rs`, `tests/finder_limits.rs`, `tests/finder_deadlines.rs` |
| Native transport | Video/audio/metadata round trips and cross-version peers; `tests/borrowed_media_sdk.rs`, `tests/metadata_roundtrip.rs`, `tests/sdk_peers.rs` |
| Receiver diagnostics | Queue-drop totals and FrameSync reception through startup, silence, pause, reconnect and disappearance; `tests/receiver_drop_totals.rs`, `tests/framesync_reception_lifecycle.rs` |
| Advanced operations | Header-family fallback, queued sends, compressed fixture decode and controls; `tests/advanced_headers.rs`, `tests/advanced_runtime.rs`, `tests/extended_lifetimes.rs` |
| Recording | Source endpoint selection, exact-path races, lifecycle and events; `tests/file_source_selection.rs`, `tests/file_exact_race_sdk.rs`, `src/file/sdk_tests.rs`, `src/file/lifecycle_tests.rs` |
| Image conversion | Native color comparisons and wrapper encoding/layout checks; `tests/image_color_sdk.rs`, `src/image/tests.rs` |

Native prerequisites are explicit: the shared test policy reports missing
runtimes, File libraries and decoders as skips, or fails when the corresponding
`GRAFTON_NDI_REQUIRE_RUNTIME` promise is set. Expected runtime identity is
checked separately from build-header selection. See
[CI prerequisites](../ci/README.md#what-a-row-promises).

A green SDK-free run can include skipped native tests. Neither a fixture decode
nor loopback media exchange qualifies hardware devices, a venue network,
third-party compressed streams or licence entitlements. A capability reports
wrapper/runtime availability, not those external properties.

## Compatibility safeguards and additional runtime checks

Capabilities combine exported functions, recognized runtime versions, SDK edition
and the Advanced declarations compiled into the crate. The registry and checks
live in [`capabilities.rs`](../src/capabilities.rs). Unsupported operations return
`Error::UnsupportedCapability`; inspect its reason rather than treating every
failure as an old runtime. Advanced header families are selected separately, so
one missing family need not disable unrelated families. Synthetic partial-header
tests check that fallback when Advanced headers are available for the build.

### Reads past the end of sent video

The wrapper reserves initialized send room for UYVY/UYVA and stages borrowed
frames unless their direct-send contract is explicitly met. Its room calculation
also accounts for non-progressive layouts. These are current defensive rules in
[`PixelFormat::send_tail_room`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/frames/enum.PixelFormat.html#method.send_tail_room)
and the sender; they do not prove that an arbitrary SDK binary stays within that
room. `tests/send_tail_room_sdk.rs` uses guard pages, and
`tests/video_padding_sdk.rs` checks padding-sensitive pictures. Run them against
the intended runtime when changing send layout or adopting an SDK build.

### Discovery Server objects

The capability registry requires NDI 6.4.0 or later for advertisers and listeners.
`tests/discovery_server.rs` exercises the configured server interaction. This gate
does not establish that all native object lifecycles are safe. Avoid rapid
unregister/register churn and qualify shutdown under application load; see the
[current API precautions](https://grantsparks.github.io/grafton-ndi/grafton_ndi/discovery/index.html).
Historical incident frequencies and timing thresholds are not portable guarantees.

### File API distribution and runtime dependencies

`file-io` uses the included File ABI independently of `advanced`. The separate
File loader in [`src/sys/file.rs`](../src/sys/file.rs) enforces its own runtime
compatibility and complete function families; the POSIX loader requires the
Advanced runtime. `tests/file_loading.rs` covers loader behavior. This source
check establishes wrapper policy, not the contents or dependencies of a vendor
archive. For a new SDK package, inspect its binaries with `readelf -d`, `otool -L`
or `dumpbin /DEPENDENTS` and archive the output with the package hash.

The recorder exposes owned state/event views through `last_accepted_source` and
`reported_current_file`; it does not call the native borrowed source/path getters.
Those native strings have no lifetime the wrapper can stabilize against a native
recording thread. A closed event is a native report: inspect `finish` and the
session report, provide sufficient storage and read back recordings whose
completeness matters. See the [File guide](file-io.md) for the workflow.

### Image encoding

`tests/image_color_sdk.rs` compares selected native conversions and metadata
handling. Its assertions apply to the cases tested, not every format or runtime.
The wrapper's color policy, layouts and encoder options are described in the
[image guide](image-encoding.md); measure application costs using the
[performance guide](image-encoding-performance.md).

### Image color conversion

The implementation in [`src/image/color.rs`](../src/image/color.rs) and its
reference tests define the wrapper conversion. Native conversion and metadata
propagation must be qualified independently for each SDK build. Specify a color
policy explicitly when the source's intended interpretation differs from the
wrapper default; a passing native comparison does not establish the intended
colorimetry of an untagged camera feed.

## Repeating the tests

Use the [native test instructions](../ci/README.md#running-the-checks-locally)
for pinned SDK provisioning, identity variables, required prerequisites, peer
runtimes and isolation. Follow [CONTRIBUTING](../CONTRIBUTING.md#checks-before-review)
for static checks. Select relevant test targets from the table above rather than
copying platform matrices and provisioning commands into this page.

Keep the checked-in bindings, expected compiler-error snapshots, codec fixtures
and generator, lockfile and compatibility pins: current builds/tests consume
them. Rebuilding an input is different from preserving the exact input used in a
qualification run.

## Recording evidence

A retained empirical claim needs an immutable run/log reference containing:

- Repository commit, dirty-tree status, target, OS, Rust version, command,
  features and relevant environment selection.
- SDK archive identity/hash, loaded library path/hash, raw runtime version,
  edition, build-header SDK and any separate File library or decoder.
- Test result, skips, required-runtime promises, stdout/stderr and isolation
  status. Record unavailable prerequisites explicitly.
- For safety probes: tool version/options and complete diagnostics. For a
  performance result: hardware, inputs/hashes, harness revision, build profile,
  warmup, sample count and which costs were included.

Scope conclusions to those observations and retain raw evidence while published
conclusions depend on it. A test definition is source evidence for its assertions;
a successful native run requires execution evidence. Remove unsupported precision
or describe it as unqualified until that evidence is available. When changing
SDKs, update the [pins and CI matrix](../ci/README.md#the-sdk-job), run the relevant
probes and record a new qualification rather than carrying forward old results.
