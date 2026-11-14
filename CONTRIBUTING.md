# Contributing to grafton-ndi

See [setup](docs/setup.md#build-requirements) for build prerequisites and
[examples](examples/README.md) for runnable uses of the library. The minimum
Rust version is 1.88; `rust-toolchain.toml` selects stable contributor tooling.

```sh
cargo build --locked
cargo test --locked
```

Native tests report a skip when their runtime is unavailable. See
[testing from source](ci/README.md) for all-feature checks, runtime selection
and loopback-only test runs.

When reporting a bug, include a small reproducer, crate version, Cargo features,
platform and native runtime version/edition. For fixes, add a regression test
and explain the change in behavior. Keep native ownership and synchronization
arguments beside unsafe operations.

Update API documentation and examples when behavior changes, and add migration
notes when callers must change. Document measured performance with its workload
and limits; test-run logs and internal audit reports do not belong in the source
tree.

Contributions use the repository's [Apache-2.0 license](LICENSE).
