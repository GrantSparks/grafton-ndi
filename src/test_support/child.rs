//! Re-execute an exact test when it needs its own process environment.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub struct TestChild {
    command: Command,
}

// Integration binaries include this module individually and use only the
// configuration and execution modes their tests need.
#[allow(dead_code)]
impl TestChild {
    pub fn new(name: &str) -> Self {
        Self::at(std::env::current_exe().expect("test executable path"), name)
    }

    /// Select a copy of the test binary for tests of loader search paths.
    pub fn at(executable: impl AsRef<OsStr>, name: &str) -> Self {
        let mut command = Command::new(executable);
        command.args(["--exact", name, "--nocapture"]);
        Self { command }
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.env(key, value);
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.command.env_remove(key);
        self
    }

    pub fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.command.current_dir(directory);
        self
    }

    /// Lend the command to a test that communicates with a live child.
    pub fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    #[track_caller]
    pub fn run(&mut self) -> Output {
        self.run_expecting(true)
    }

    #[track_caller]
    pub fn run_expecting(&mut self, success: bool) -> Output {
        let output = self.output(success);
        if output.status.success() {
            forward_skips(&output, &mut io::stderr().lock());
        }
        output
    }

    #[track_caller]
    pub(super) fn output(&mut self, success: bool) -> Output {
        let output = self
            .command
            .output()
            .unwrap_or_else(|error| panic!("could not run {:?}: {error}", self.command));
        self.check(&output, success);
        output
    }

    #[track_caller]
    pub fn run_with_timeout(&mut self, timeout: Duration) {
        let mut child = self
            .command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("could not start {:?}: {error}", self.command));
        let deadline = Instant::now() + timeout;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{:?} did not complete within {timeout:?}:\n{}\n{}",
                    self.command,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        self.check(&output, true);
        forward_skips(&output, &mut io::stderr().lock());
    }

    #[track_caller]
    fn check(&self, output: &Output, success: bool) {
        assert_eq!(
            output.status.success(),
            success,
            "{:?} exited with {}:\n{}\n{}",
            self.command,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

pub(super) fn forward_skips(output: &Output, stderr: &mut impl Write) {
    let errors = String::from_utf8_lossy(&output.stderr);
    let (notices, reasons): (Vec<_>, Vec<_>) = errors
        .lines()
        .filter(|line| !line.is_empty())
        .partition(|line| line.starts_with("skipped:"));
    if !notices.is_empty() {
        for line in reasons.into_iter().chain(notices) {
            let _ = writeln!(stderr, "{line}");
        }
    }
}
