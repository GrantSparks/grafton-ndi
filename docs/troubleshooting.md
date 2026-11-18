# Troubleshoot NDI

## Runtime loading

Match typed selection causes. `RuntimeLoadFailed(failure)` retains the search
attempts with their paths, origins and refusals. When `failure.loaded_any()` is
true, native code loaded but was refused; inspect `attempt.refusal()` for a
missing entry point, incompatible edition, invalid version or other cause.
When it is false, no candidate could be opened; platform-loader diagnostics
can also describe problems other than an absent file. `RuntimeConflict(failure)`
exposes its `SelectionConflict` through `failure.kind()` and retains the library
and paths involved. Its `Display` gives the diagnostic text.

For `RuntimeLoadFailed`, inspect the attempted paths and verify the library's
architecture and dependencies. Consult the
[runtime search policy](https://grantsparks.github.io/grafton-ndi/grafton_ndi/runtime/index.html#where-the-runtime-is-loaded-from)
or the [File library search policy](https://grantsparks.github.io/grafton-ndi/grafton_ndi/file/index.html#where-the-file-library-is-loaded-from),
depending on the diagnostic's library.

For `RuntimeConflict(failure)`, inspect `failure.kind()`, `selected()`,
`requested()` and `foreign_module()` alongside the selected runtime edition.
A successful selection lasts for the process, so configure it before another component
initializes NDI. Restart after changing the selected library.

`SelectionConflict::ForeignRuntimeInScope` or
`LoadRefusal::ForeignRuntimeInScope` means a runtime already linked or preloaded
into the process's global symbol scope would intercept another runtime's native
calls. On non-Apple ELF targets, select the file named by the diagnostic, or
remove the other runtime's direct link or preload. Matching its version or
edition does not make two different runtime modules interchangeable. A Standard
runtime in scope also prevents selecting a separate Advanced runtime.

For `UnsupportedCapability`, inspect its `capability` (`Runtime(feature)` or
`File(family)`) and typed `refusal` (`reason()` gives its category), then check
[runtime capabilities](setup.md#runtime-capabilities). A Cargo feature, SDK
edition or exported entry point alone does not establish hardware support or
license entitlement.

## SDK limitations

Native behavior needs validation with the runtime, platform and network you
deploy. Use [runtime checks](../ci/README.md#test-against-an-installed-runtime)
with your installation. Older incident reports do not establish that a different
runtime is safe.

## Discovery Server

Advertisers and listeners require NDI 6.4 or later; the wrapper rejects older
versions. For deployment testing, exercise advertiser handover, server startup
and reconnection, listener teardown, and receiver connection changes. Avoid
rapid re-registration of the same source, and keep the server reachable during
these operations. See the [Discovery Server API](https://grantsparks.github.io/grafton-ndi/grafton_ndi/discovery_server/index.html).

## Receiving and sending

- **No media arrives:** verify source discovery, network reachability and the
  sender's active media. A connection count alone is insufficient; use the
  [per-media reception monitor](usage.md#monitor-reception-of-the-expected-media).
  For compressed sources, check decoder availability for the actual codec and
  format on the destination machine.
- **Clocked output looks alive after input stops:** FrameSync can supply retained
  video or silent audio. Monitor incoming media separately when freshness matters.
  A FrameSync owns its receiver; its restricted receiver handle excludes raw
  audio capture, which would compete for its input.
- **A wait or shutdown stalls:** Genlock waits have no cancellation or deadline.
  Dropping a queued sender waits for queued frames to complete. Dropping a future
  cannot interrupt native work that already started. Keep these operations away
  from threads that must remain responsive.
- **Format differs from the sender:** validate the received format and picture
  content in the deployment test; a requested input layout does not promise
  preservation through native encoding and decoding.

## File library

Provision recording space, inspect `finish` and its report, and read recordings
back when completeness matters. Include full-disk, source-switch and shutdown
cases in testing. See [File library limits](file-io.md#limits-of-the-ndi-64-file-library)
for the wrapper's guarantees and precautions.
