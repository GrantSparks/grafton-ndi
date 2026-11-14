# Contributing to grafton-ndi

Start with the [setup guide](docs/setup.md#build-requirements) for build prerequisites.
[rust-toolchain.toml](rust-toolchain.toml) selects stable Rust with Clippy, rustfmt
and rust-src for contributor tooling; the library's minimum Rust version is 1.88.

```sh
cargo build --locked
cargo test --locked
```

Tests that cannot load an NDI runtime normally report a skip. Initialization
errors still fail, and `GRAFTON_NDI_REQUIRE_RUNTIME` turns missing promised
prerequisites into failures. Clear runtime expectation variables when switching
SDKs. See [local checks and native tests](ci/README.md#running-the-checks-locally)
for provisioning, runtime identity, isolation and the full check commands.
Real-runtime tests create network-visible NDI objects; use the documented
isolated runner where appropriate.

## Checks before review

Run the checks appropriate to the change in the [development guide](docs/development.md#checks-before-review).
Report the commands, platform, SDK edition/version and skipped prerequisites.
A configured test or a skipped native test is not evidence that a runtime passed.

## Pull requests and documentation

Explain the concrete trigger, previous behavior and resulting behavior. Add a
regression that fails for the original defect. For FFI changes, cite the SDK
contract and put the lasting safety argument beside the unsafe operation.
See [implementation and safety contracts](docs/development.md).

For public API changes, update rustdoc, affected examples, the
[changelog](CHANGELOG.md) and [migration guide](docs/migration/README.md) when
callers must change. Verify defaults, errors, capability gates, ownership,
threading, allocation and cancellation claims against the implementation and
relevant tests. Use runnable examples or compile-checked rustdoc for code.

Keep each fact in one maintained home: setup for user prerequisites, usage and
feature guides for workflows, rustdoc for exact method contracts, CI for test
operations, and development for implementation rules. Keep release comparisons
in the changelog and migration guides. New empirical claims need reproducible
[evidence](docs/sdk-validation.md), with their scope and limitations.

Contributions use the repository's [Apache-2.0 license](LICENSE).
