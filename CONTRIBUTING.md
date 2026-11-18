# Contributing to grafton-ndi

See [setup](docs/setup.md#build-requirements) for build prerequisites and
[examples](examples/README.md) for runnable uses of the library. The minimum
Rust version is 1.88; `rust-toolchain.toml` selects the pinned contributor toolchain.

```sh
cargo build --locked
cargo test --locked
```

The test suite runs from the repository. Test-only sources are not published
in the crate package.

Native tests report a skip when their runtime is unavailable. See
[testing from source](ci/README.md) for all-feature checks, runtime selection
and loopback-only test runs.

When reporting a bug, include a small reproducer, crate version, Cargo features,
platform and native runtime version/edition. For fixes, add a regression test
and explain the change in behavior. Keep native ownership and synchronization
arguments beside unsafe operations.

Production code may panic only through an `#[expect]` whose reason states the
invariant that justifies the site. Use `const_assert!` for compile-time assertions
and reserve `assert!`, `assert_eq!` and `assert_ne!` for tests.

Update API documentation and examples when behavior changes, and add migration
notes when callers must change. Follow the [public API conventions](docs/api-conventions.md)
for names, signatures, traits, ownership and documentation. Keep issue investigations, implementation plans,
audit reports and measured results on the relevant GitHub issue. Include the
workload, environment and limits with performance results; repository docs
should explain how to use and maintain the library.

Keep build outputs, test logs, recordings and reproducible fixture outputs out
of Git. Commit generators and generate their outputs in temporary directories
or under `target/`. Compile-failure `.stderr` files are reviewed expectations
used to detect regressions, so keep them with their test cases. The vendored SDK
declarations are shipped source needed to build without an installed SDK.

Contributions use the repository's [Apache-2.0 license](LICENSE).
