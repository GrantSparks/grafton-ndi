# Troubleshoot NDI

## Runtime loading

Use absolute paths for `library_path` and `NDI_RUNTIME_DIR_V6`. A relative
explicit path returns `Error::InvalidInput` before selection on every target.
A relative environment directory is recorded as refused and never opened;
the search continues with the platform loader.

For `RuntimeLoadFailed`, inspect the attempted paths and errors, then check the
[runtime search order](setup.md#finding-the-runtime). An absolute
`RuntimeOptions::library_path` selects one file without fallback. Verify its
architecture and dependencies as well as its name.

For `RuntimeConflict`, inspect the selected path and edition. A successful
selection lasts for the process, so configure it before another component
initializes NDI. Restart after changing the selected library.

For `UnsupportedCapability`, inspect its feature and reason, then check
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
these operations. See the [Discovery Server API](https://grantsparks.github.io/grafton-ndi/grafton_ndi/discovery/index.html).

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
