//! Compile the fake NDI runtime in `fake_runtime.rs` for the target under test.
//!
//! Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

const SUPPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support");

/// One variant of the fake runtime library.
#[derive(Default)]
pub struct FakeRuntime<'a> {
    cfgs: Vec<&'static str>,
    version: Option<&'a str>,
    dependency: Option<&'a Path>,
}

impl<'a> FakeRuntime<'a> {
    /// The plain fake: every required function, nothing optional.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable a `--cfg` variant documented in `fake_runtime.rs`.
    pub fn with(mut self, cfg: &'static str) -> Self {
        self.cfgs.push(cfg);
        self
    }

    /// Report `version` (for example `"6.4.0.2"`) as the runtime's version text.
    pub fn version(mut self, version: &'a str) -> Self {
        self.version = Some(version);
        self
    }

    /// Link the fake against `library`, loaded from the fake's own directory.
    pub fn depending_on(mut self, library: &'a Path) -> Self {
        self.dependency = Some(library);
        self
    }

    /// Compile the variant to `output`.
    pub fn build(self, output: &Path) -> PathBuf {
        let mut rustc = rustc("fake_runtime.rs", output);
        for cfg in self.cfgs {
            rustc.args(["--cfg", cfg]);
        }
        if let Some(version) = self.version {
            rustc.env("FAKE_NDI_VERSION", format!("NDI SDK TEST {version}"));
        }
        if let Some(dependency) = self.dependency {
            rustc
                .arg("-Clink-arg=-Wl,--no-as-needed")
                .arg(link_argument(dependency.as_os_str()))
                .arg("-Clink-arg=-Wl,-rpath,$ORIGIN");
        }
        run(rustc, output)
    }
}

/// Compile the library that defines a single foreign NDI symbol.
pub fn build_dependency(output: &Path) -> PathBuf {
    run(rustc("fake_dependency.rs", output), output)
}

fn rustc(source: &str, output: &Path) -> Command {
    let mut rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc
        .args(["--edition=2021", "--crate-type=cdylib", "-Cpanic=abort"])
        .args(["--target", env!("GRAFTON_NDI_TARGET")])
        .arg("-o")
        .arg(output)
        .arg(Path::new(SUPPORT).join(source));
    rustc
}

fn link_argument(library: &OsStr) -> std::ffi::OsString {
    let mut argument = std::ffi::OsString::from("-Clink-arg=");
    argument.push(library);
    argument
}

fn run(mut rustc: Command, output: &Path) -> PathBuf {
    let result = rustc.output().expect("run rustc for the fake NDI runtime");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output.to_owned()
}
