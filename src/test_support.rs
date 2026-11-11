//! Support for tests that need a real NDI runtime.
//!
//! The crate's unit tests use this module directly and its integration tests
//! include the same file, so both follow one policy.

use std::{io::Write, sync::Once};

use super::{Error, NDI};

/// Setting this variable to `1` turns a missing runtime into a test failure.
const REQUIRE_RUNTIME: &str = "GRAFTON_NDI_REQUIRE_RUNTIME";

/// An initialized runtime, or `None` when no NDI runtime library can be loaded.
///
/// A test that needs the real SDK starts with
/// `let Some(ndi) = ndi() else { return };` so it runs wherever a runtime is
/// installed and is visibly skipped elsewhere. Automation that must not skip
/// sets `GRAFTON_NDI_REQUIRE_RUNTIME=1`. Every failure other than a missing
/// runtime library panics.
pub fn ndi() -> Option<NDI> {
    match NDI::new() {
        Ok(ndi) => Some(ndi),
        Err(Error::RuntimeLoadFailed(reason)) => {
            if std::env::var_os(REQUIRE_RUNTIME).is_some_and(|value| value == "1") {
                panic!("{REQUIRE_RUNTIME}=1 but no NDI runtime could be loaded: {reason}");
            }
            report_skip(&reason);
            None
        }
        Err(error) => panic!("the NDI runtime loaded but did not initialize: {error}"),
    }
}

/// Name the skipped test, and say once per process why no runtime loaded.
///
/// This writes to standard error directly: the test harness captures
/// `eprintln!` output of passing tests, and a skip must stay visible.
fn report_skip(reason: &str) {
    static EXPLAINED: Once = Once::new();
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    let mut stderr = std::io::stderr().lock();
    EXPLAINED.call_once(|| {
        let _ = writeln!(stderr, "no NDI runtime could be loaded: {reason}");
    });
    let _ = writeln!(stderr, "skipped: {test} needs an NDI runtime");
}
