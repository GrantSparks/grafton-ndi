# Set up NDI

## Build requirements

Use Rust 1.88 or later. Default Windows, macOS, Linux and Android builds need no
NDI SDK. Two configurations need additional inputs:

- Advanced native operations need the `advanced` feature, Advanced SDK headers
  and `libclang`. Set `NDI_SDK_DIR` to the SDK root; see the [Advanced guide](advanced.md).
  Without Advanced declarations the API compiles, but its operations are unavailable.
- iOS, tvOS and visionOS link a vendor archive. See [Apple mobile targets](#apple-mobile-targets)
  for SDK and toolchain requirements.

To run a dynamically loaded build, obtain an
[NDI 6 runtime](https://ndi.video/for-developers/ndi-sdk/) and put its library
beside the executable, or set `NDI_RUNTIME_DIR_V6` to its absolute directory. For example:

```sh
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
```

`NDI_SDK_DIR` selects build inputs; `NDI_RUNTIME_DIR_V6` selects a runtime search
directory. An explicit `NDI_SDK_DIR` must be valid in builds that read it. Unset
stale values. After installing an SDK in a default location, set `NDI_SDK_DIR`
or run `cargo clean -p grafton-ndi` to trigger a rebuild.

## Finding the runtime

`Ndi::new()` loads the first usable library from the executable's directory,
an absolute `NDI_RUNTIME_DIR_V6`, or the platform's loader locations, in that
order. Use `RuntimeOptions::library_path` to select an existing absolute file.
See the [runtime search and trust policy](https://grantsparks.github.io/grafton-ndi/grafton_ndi/runtime/index.html#where-the-runtime-is-loaded-from)
for edition priority, platform details and path refusals.

| Platform | Standard name | Advanced name |
| --- | --- | --- |
| Windows x64 | `Processing.NDI.Lib.x64.dll` | `Processing.NDI.Lib.Advanced.x64.dll` |
| Windows x86 | `Processing.NDI.Lib.x86.dll` | `Processing.NDI.Lib.Advanced.x86.dll` |
| macOS | `libndi.dylib` | `libndi_advanced.dylib` |
| Linux | `libndi.so.6` | `libndi_advanced.so.6` |
| Android | `libndi.so` | `libndi_advanced.so` |

## Selecting a runtime

`Runtime::select` loads and inspects a library before starting NDI. `initialize`
returns an `Ndi` handle; `UnsupportedCpu` identifies initialization failure on
an unsupported CPU. `Ndi::new()` performs both steps with defaults;
`Ndi::with_options` uses your options.

```rust
use grafton_ndi::{Runtime, RuntimeOptions};

fn main() -> grafton_ndi::Result<()> {
    let runtime = Runtime::select(&RuntimeOptions::default())?;
    println!("NDI {} from {:?}", runtime.info().raw_version(), runtime.info().library_path());
    let ndi = runtime.initialize()?;
    println!("edition: {:?}", ndi.info().edition());
    Ok(())
}
```

## SDK editions

On dynamically loaded targets, search without an edition request accepts either
library. Use
`RuntimeOptions::new().edition(SdkEdition::Advanced)` to search only Advanced
names and require Advanced entry points. `SdkEdition::Standard` searches Standard
names, but an Advanced runtime found under that name or an explicit path also
satisfies the request. On these targets, installed build inputs do not choose
the runtime, and `ndi.info().edition()` reports the loaded library's edition
from its exports.

On statically linked Apple mobile targets, `ndi.info().edition()` reports the
linked archive's edition. The build selects the Advanced archive when
`NDI_SDK_DIR` holds one, otherwise the Standard archive; see
[Apple mobile targets](#apple-mobile-targets). An Advanced edition request is
met only by an Advanced archive, while a Standard request accepts either.
The reported edition does not establish license entitlement.

## Runtime capabilities

Cargo features expose Rust APIs; `ndi.capabilities()` reports optional native
operations. Use `ndi.require_feature(RuntimeFeature::ReceiveDelay)?` to require
one. `UnsupportedCapability` identifies `Capability::Runtime(feature)` and a typed
`CapabilityRefusal`, including the missing symbol or required version. Its
`reason()` gives the refusal category; see
[`CapabilityUnavailable`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/capabilities/enum.CapabilityUnavailable.html).
Availability does not establish hardware support or license entitlement.

Discovery Server advertisers and listeners require NDI 6.4 or later.
`RuntimeFeature::RemoteReceiverControl` permits Discovery Server clients to
change an advertised receiver's source. On Windows this requires an Advanced
runtime. Standard Windows receivers can still be advertised for monitoring
and input-group membership. Other operations may depend on exported entry points, version, edition and build
headers. Query the selected runtime instead of inferring support from a version
matrix. The separate File library reports its families through `FileApi::status`
and `supports`, or validates them with `require_family`. Missing families use
the same `UnsupportedCapability` variant with `Capability::File(family)`.
See the [File guide](file-io.md).

Selection failures are structured. `RuntimeLoadFailed(failure)` exposes each
attempt's location, `LoadOrigin`, and `LoadRefusal`; `failure.loaded_any()`
distinguishes libraries that could not be opened from ones loaded and refused.
`RuntimeConflict(failure)` exposes `kind()` (`SelectionConflict`), `library()`,
`selected()`, `requested()` and `foreign_module()` for a library mismatch,
edition mismatch, linked implementation or other conflict. Use these typed
fields for decisions and `Display` for diagnostics.

## Deploy an application

Include the runtime library and any native dependencies in your deployment.
For File applications, include the companion File library and follow the
[File library requirements](file-io.md#requirements). Check the selected
paths through `RuntimeInfo::library_path` and `FileApi::library_path`.

## Supported NDI versions and platforms

The crate targets NDI 6 Standard and Advanced runtimes. Checked-in core bindings
come from the 6.4.0.2 headers. CI is configured for Standard 6.1.1 and Standard
and Advanced 6.3.2/6.4.0.2, with the target coverage below. Other NDI releases
have no configured runtime coverage; NDI 5 and earlier are outside the support
policy. This is a coverage description, not a record of successful runs. See the
[CI workflow](../.github/workflows/rust.yml) for the exact package/job matrix and [GitHub Actions](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
for execution results.

### NDI versions

Use capability queries for optional APIs and test your deployment. Cross-version
CI jobs are configured to exercise alternate header/runtime versions and media
exchange between versions; their workflow alone does not prove interoperability
for an application.

### Platforms

“Runtime tests” below means jobs configured to load a real native runtime.
“Compile/lint” and “build” do not load a runtime or establish device behavior.

| Platform | Rust targets | Configured coverage |
| --- | --- | --- |
| Linux (glibc) | `x86_64-unknown-linux-gnu` | Runtime tests: Standard 6.1.1, 6.3.2, 6.4.0.2; Advanced 6.3.2, 6.4.0.2 |
| Linux (glibc) | `i686-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | Runtime tests: Standard 6.4.0.2 |
| Linux (glibc) | `armv7-unknown-linux-gnueabihf` | Compile/lint |
| Windows (MSVC) | `x86_64-pc-windows-msvc` | Runtime tests: Standard and Advanced 6.3.2, 6.4.0.2 |
| Windows (MSVC) | `i686-pc-windows-msvc` | Runtime tests: Advanced 6.4.0.2 |
| macOS | `aarch64-apple-darwin` | Runtime tests: Standard and Advanced 6.3.2, 6.4.0.2 |
| macOS | `x86_64-apple-darwin` | Runtime tests: Standard 6.4.0.2 |
| Android | `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android` | Compile/lint |
| iOS | `aarch64-apple-ios`, `x86_64-apple-ios` (simulator) | Static build, Apple SDK 6.4 |
| tvOS | `aarch64-apple-tvos`, `x86_64-apple-tvos` (simulator) | Static build, Apple SDK 6.4 |
| visionOS | `aarch64-apple-visionos` | Static build, Apple SDK 6.4 |

Dynamically loaded targets need a runtime for their architecture. The Apple
mobile build supports the device/simulator combinations above; ARM64 Apple
simulators and Mac Catalyst are not supported. Windows ARM64 and unlisted
targets (including musl, BSD and Windows GNU) are outside configured coverage.

### Apple mobile targets

Set `NDI_SDK_DIR` to an NDI SDK for Apple. The build selects its Standard or
Advanced archive and validates its CPU slice, platform and deployment floor.
With `file-io`, it selects the File archive when available. `library_path` is
rejected with `RuntimeConflict` when absolute. Relative paths return
`InvalidInput` before selection on every target; selection loads no dynamic
library.

Use the SDK's required deployment target or later. The CI script sets iOS/tvOS
16.0 and visionOS 1.0 for its 6.4 archives. Explicit `IPHONEOS_DEPLOYMENT_TARGET`,
`TVOS_DEPLOYMENT_TARGET` or `XROS_DEPLOYMENT_TARGET` values below the selected
archive's floor fail the build. The application linker also checks deployment
compatibility.

Rust 1.88 is the crate minimum. CI lints iOS, tvOS and visionOS with the current
stable toolchain, except that the x86-64 tvOS simulator uses a pinned nightly
with `-Zbuild-std`. Both default and all-feature builds deny warnings; see
[`ci/apple-mobile.sh`](../ci/apple-mobile.sh) for exact commands.
