//! Compile the fake NDI runtime in `fake_runtime.rs` for the target under test.
//!
//! Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const SUPPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support");

/// A directory for one test run's fixtures.
///
/// Its name is taken exclusively, so runs that share a temporary directory,
/// even with the same process identifier in different namespaces, cannot use
/// or remove each other's files. It is removed when dropped, whether the test
/// passed or panicked.
pub struct FixtureDir(PathBuf);

impl FixtureDir {
    /// Create a directory no other run has, named after `purpose`.
    pub fn create(purpose: &str) -> Self {
        let mut attempt = 0u32;
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = std::env::temp_dir().join(format!(
                "grafton-ndi-{purpose}-{}-{nanos:09}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        // Nothing can be done about a failure here, and a leftover directory
        // under the temporary directory harms no later run.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One variant of the fake runtime library.
#[derive(Default)]
pub struct FakeRuntime<'a> {
    cfgs: Vec<&'static str>,
    version: Option<&'a str>,
    dependency: Option<&'a Path>,
    soname: Option<&'a str>,
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

    /// Record `soname` in the library as the name the platform loader knows it by.
    pub fn soname(mut self, soname: &'a str) -> Self {
        self.soname = Some(soname);
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
        if let Some(soname) = self.soname {
            rustc.arg(format!("-Clink-arg=-Wl,-soname,{soname}"));
        }
        run(rustc, output)
    }
}

/// Compile the library that defines a single foreign NDI symbol.
pub fn build_dependency(output: &Path) -> PathBuf {
    run(rustc("fake_dependency.rs", output), output)
}

/// A compiler invocation that builds `source`, a file in this directory, into
/// the shared library `output` for the target under test.
pub fn rustc(source: &str, output: &Path) -> Command {
    let mut rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc
        .args(["--edition=2021", "--crate-type=cdylib", "-Cpanic=abort"])
        .args(["--target", env!("GRAFTON_NDI_TARGET")])
        .arg("-o")
        .arg(output)
        .arg(Path::new(SUPPORT).join(source));
    rustc
}

/// The compiler argument that links against `library`.
pub fn link_argument(library: &OsStr) -> std::ffi::OsString {
    let mut argument = std::ffi::OsString::from("-Clink-arg=");
    argument.push(library);
    argument
}

/// Run a compiler invocation that must succeed, and return its `output`.
pub fn run(mut rustc: Command, output: &Path) -> PathBuf {
    let result = rustc.output().expect("run rustc for the fake NDI runtime");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output.to_owned()
}
