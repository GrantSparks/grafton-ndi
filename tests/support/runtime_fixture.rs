//! Compile a selected fake NDI runtime for the target under test.
//!
//! Options are data rather than per-variant builder methods: each test names
//! only the settings it needs, and every field is handled by `build`.

use std::path::{Path, PathBuf};

#[path = "compiler.rs"]
mod compiler;
pub use compiler::{link_argument, run, rustc};

/// One variant of the fake runtime library.
#[derive(Default)]
pub struct FakeRuntime<'a> {
    /// The `--cfg` variants documented in `fake_runtime.rs`.
    pub cfgs: Vec<&'static str>,
    /// The version text to report, for example `"6.4.0.2"`.
    pub version: Option<&'a str>,
    /// A dependency loaded from the fake runtime's own directory.
    pub dependency: Option<&'a Path>,
    /// The name the platform loader should record for this library.
    pub soname: Option<&'a str>,
}

impl FakeRuntime<'_> {
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
        if let Some(soname) = self.soname {
            rustc.arg(format!("-Clink-arg=-Wl,-soname,{soname}"));
        }
        run(rustc, output)
    }
}

/// Read the log named by the test, which may still be waiting for its first event.
#[allow(dead_code)]
pub fn read_fake_log() -> String {
    let path = fake_log_path();
    match std::fs::read_to_string(&path) {
        Ok(log) => log,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("cannot read fake runtime log {}: {error}", path.display()),
    }
}

/// The configured log path, also used when asserting the SDK has not yet created it.
pub fn fake_log_path() -> PathBuf {
    PathBuf::from(
        std::env::var_os("GRAFTON_FAKE_NDI_LOG").expect("the test must set GRAFTON_FAKE_NDI_LOG"),
    )
}
