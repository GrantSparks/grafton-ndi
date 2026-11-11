//! Compile loadable test fixtures for the test process, including x86 tests on
//! x86_64 runners whose default C compiler otherwise emits 64-bit libraries.

use std::process::Command;

pub fn command() -> Command {
    let mut compiler = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
    if cfg!(all(target_os = "linux", target_arch = "x86")) {
        compiler.arg("-m32");
    }
    compiler
}
