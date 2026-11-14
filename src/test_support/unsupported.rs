//! Report a platform limitation separately from a missing prerequisite.

use std::io::Write;

/// Record that the calling test leaves something out because this platform or
/// runtime cannot do it: `what` names it. Unlike [`skip`], no environment can
/// promise otherwise.
pub fn unsupported(what: &str) {
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    // Written to standard error directly, as `skip` writes.
    let _ = writeln!(std::io::stderr().lock(), "skipped: {test}: {what}");
}
