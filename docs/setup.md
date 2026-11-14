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
beside the executable, or set `NDI_RUNTIME_DIR_V6` to its directory. For example:

```sh
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
```

`NDI_SDK_DIR` selects build inputs; `NDI_RUNTIME_DIR_V6` selects a runtime search
directory. An explicit `NDI_SDK_DIR` must be valid in builds that read it. Unset
stale values. After installing an SDK in a default location, set `NDI_SDK_DIR`
or run `cargo clean -p grafton-ndi` to trigger a rebuild.

## Finding the runtime

`Ndi::new()` selects and initializes a runtime. The dynamic loader tries:

1. The executable's directory.
2. `NDI_RUNTIME_DIR_V6`.
3. The platform loader's search path (`rpath`, `LD_LIBRARY_PATH`, `PATH` and system directories).

At each location it tries both edition names unless you [request one](#sdk-editions).
Builds with `advanced`, or with `file-io` on Linux, macOS or Android, try Advanced
first; other builds try Standard first. macOS first tries `@rpath/<name>` in
its platform search. Missing, unloadable or incompatible candidates do not stop
the search.

| Platform | Standard name | Advanced name |
| --- | --- | --- |
| Windows x64 | `Processing.NDI.Lib.x64.dll` | `Processing.NDI.Lib.Advanced.x64.dll` |
| Windows x86 | `Processing.NDI.Lib.x86.dll` | `Processing.NDI.Lib.Advanced.x86.dll` |
| macOS | `libndi.dylib` | `libndi_advanced.dylib` |
| Linux | `libndi.so.6` | `libndi_advanced.so.6` |
| Android | `libndi.so` | `libndi_advanced.so` |

An absolute `RuntimeOptions::library_path` tries only that file. Relative paths
return `Error::InvalidInput`. `RuntimeLoadFailed` lists attempted paths and errors;
failed selection can be retried. A successful selection lasts for the process.
A different path or incompatible edition then returns `RuntimeConflict`.

Rejected libraries that loaded also stay resident. A platform search may resolve
to the rejected module again; remove or fix that library, or select an exact path.

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

Without an edition request, search accepts either library. Use
`RuntimeOptions::new().edition(SdkEdition::Advanced)` to search only Advanced
names and require Advanced entry points. `SdkEdition::Standard` searches Standard
names, but an Advanced runtime found under that name or an explicit path also
satisfies the request. Installed build inputs do not choose the runtime.

`ndi.info().edition()` reports the loaded library's edition from its exports;
it does not establish license entitlement.

## Runtime capabilities

Cargo features expose Rust APIs; `ndi.capabilities()` reports optional native
operations. Use `ndi.require_feature(RuntimeFeature::ReceiveDelay)?` to require
one. `UnsupportedCapability` identifies its feature, reason and detail; see
[`CapabilityUnavailable`](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/capabilities/enum.CapabilityUnavailable.html).
Availability does not establish hardware support or license entitlement.

Discovery Server advertisers and listeners require NDI 6.4 or later. Other
operations may depend on exported entry points, version, edition and build
headers. Query the selected runtime instead of inferring support from a version
matrix. The separate File library has its own `FileApi::status` and `supports`;
the core reports `SeparateLibrary` even when it is installed. See the [File guide](file-io.md).

## Deploy an application

Package the runtime explicitly; a dynamically loaded library is not a static
link dependency. **Default search trusts every searched directory:** loading
native code executes its constructors before symbols or edition can be checked,
even for rejected candidates. For a service, select an absolute path in an
application-owner-controlled directory before another component selects a runtime:

```rust
use grafton_ndi::{Ndi, RuntimeOptions};

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::with_options(&RuntimeOptions::new()
        .library_path("/opt/my-service/ndi/libndi.so.6"))?;
    println!("Runtime: {:?}", ndi.info().library_path());
    Ok(())
}
```

That selection has no fallback. Native dependencies still follow platform loader
rules and need trusted locations. File API users must also select an absolute
`FileRuntimeOptions::library_path` or trust all its search directories, starting
with the core runtime's companion directory. Inspect `FileApi::library_path`;
see [File library deployment](file-io.md#requirements).

## Supported NDI versions and platforms

The crate targets NDI 6 Standard and Advanced runtimes. Checked-in core bindings
come from the 6.4.0.2 headers. CI is configured for Standard 6.1.1 and Standard
and Advanced 6.3.2/6.4.0.2, with the target coverage below. Other NDI releases
have no configured runtime coverage; NDI 5 and earlier are outside the support
policy. This is a coverage description, not a record of successful runs. See
[SDK validation](sdk-validation.md) for evidence requirements and
[CI configuration](../ci/README.md) for the exact package/job matrix.

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
rejected with `RuntimeConflict`; selection loads no dynamic library.

Use the SDK's required deployment target or later. The CI script sets iOS/tvOS
16.0 and visionOS 1.0 for its 6.4 archives. Explicit `IPHONEOS_DEPLOYMENT_TARGET`,
`TVOS_DEPLOYMENT_TARGET` or `XROS_DEPLOYMENT_TARGET` values below the selected
archive's floor fail the build. The application linker also checks deployment
compatibility.

Rust 1.88 is the crate minimum, but tvOS and visionOS use newer toolchains in CI.
The x86-64 tvOS simulator uses a pinned nightly with `-Zbuild-std`; see
[`ci/apple-mobile.sh`](../ci/apple-mobile.sh) for exact commands.
