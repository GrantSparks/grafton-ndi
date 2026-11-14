//! Wait for the first available source while allowing graceful shutdown.

use std::time::Duration;

use grafton_ndi::{Finder, Result, Source};

/// Block until at least one source is discovered, re-checking every second.
///
/// `should_stop` is polled between waits so callers can bail out on a Ctrl-C
/// flag; when it returns `true` this returns `Ok(None)`. Pass `|| false` to
/// wait indefinitely.
///
/// Unlike [`Finder::find_sources`], which enumerates the *complete* set over a
/// fixed window, this returns the moment the *first* source appears — what the
/// examples want when they only need something to connect to.
pub fn wait_for_first_source(
    finder: &Finder,
    should_stop: impl Fn() -> bool,
) -> Result<Option<Vec<Source>>> {
    loop {
        if should_stop() {
            return Ok(None);
        }
        let sources = finder.current_sources()?;
        if !sources.is_empty() {
            return Ok(Some(sources));
        }
        finder.wait_for_sources(Duration::from_secs(1))?;
    }
}
