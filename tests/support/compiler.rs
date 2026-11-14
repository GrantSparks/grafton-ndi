//! Compiler operations shared by native-library fixtures.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

const SUPPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support");

/// A compiler invocation that builds `source`, a file in this directory, into
/// the shared library `output` for the target under test.
pub fn rustc(source: &str, output: &Path) -> Command {
    let mut rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    rustc
        .args([
            "--edition=2021",
            "--crate-type=cdylib",
            "-Cpanic=abort",
            "-Dwarnings",
        ])
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
