//! Support for tests that need native NDI code.
//!
//! The crate's unit tests use this module directly and its integration tests
//! include the same file, so both follow one policy:
//!
//! * a test that needs native code asks for it first and returns when the answer
//!   is `None`, so it runs wherever that code is installed and is visibly skipped
//!   elsewhere;
//! * automation that must not skip lists what its environment provides in
//!   `GRAFTON_NDI_REQUIRE_RUNTIME`, which turns the matching skips into failures.

use std::{io::Write, sync::Mutex};

use super::{capabilities::RuntimeFeature, Error, NDI};

/// A comma-separated list of what the environment provides; see [`Requirement`].
const REQUIRE_RUNTIME: &str = "GRAFTON_NDI_REQUIRE_RUNTIME";

/// Native code a test may need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    /// Any NDI runtime library. Listed as `1`, and implied by every other entry.
    Runtime,
    /// An Advanced SDK runtime, in a build that has the Advanced SDK
    /// declarations. Listed as `advanced`.
    Advanced,
    /// The NDI File library. Listed as `file`.
    File,
    /// The vendor's Discovery Server program, installed with the SDK the
    /// selected runtime belongs to, and a runtime whose advertisers and
    /// listeners can use it. Listed as `discovery-server`.
    DiscoveryServer,
}

impl Requirement {
    const ALL: [Self; 4] = [
        Self::Runtime,
        Self::Advanced,
        Self::File,
        Self::DiscoveryServer,
    ];

    /// Its spelling in `GRAFTON_NDI_REQUIRE_RUNTIME`.
    fn token(self) -> &'static str {
        match self {
            Self::Runtime => "1",
            Self::Advanced => "advanced",
            Self::File => "file",
            Self::DiscoveryServer => "discovery-server",
        }
    }

    /// What a skipped test is reported to need.
    fn description(self) -> &'static str {
        match self {
            Self::Runtime => "an NDI runtime",
            Self::Advanced => "the Advanced SDK",
            Self::File => "the NDI File library",
            Self::DiscoveryServer => "the SDK's Discovery Server",
        }
    }

    /// Whether the environment promises to provide it.
    ///
    /// An entry that names no requirement is a configuration mistake and
    /// panics: a misspelt promise must not quietly allow skips.
    fn is_promised(self) -> bool {
        let Some(promises) = std::env::var_os(REQUIRE_RUNTIME) else {
            return false;
        };
        let promises = promises
            .to_str()
            .unwrap_or_else(|| panic!("{REQUIRE_RUNTIME} is not UTF-8"));
        let mut promised = false;
        for entry in promises.split(',').map(str::trim) {
            let named = Self::ALL
                .into_iter()
                .find(|requirement| requirement.token() == entry)
                .unwrap_or_else(|| {
                    panic!(
                        "{REQUIRE_RUNTIME} lists {entry:?}; expected 1, advanced, file or \
                         discovery-server"
                    )
                });
            promised |= named == self || self == Self::Runtime;
        }
        promised
    }
}

/// Record that the calling test is skipped because `requirement` is not met.
///
/// # Panics
///
/// Panics when `GRAFTON_NDI_REQUIRE_RUNTIME` promises the requirement.
pub fn skip(requirement: Requirement, reason: &str) {
    if requirement.is_promised() {
        panic!(
            "{REQUIRE_RUNTIME} promises {} but {reason}",
            requirement.token()
        );
    }
    // The reason is often long (every library location tried), so it is
    // printed once per requirement and each further skip only names its test.
    static EXPLAINED: Mutex<Vec<Requirement>> = Mutex::new(Vec::new());
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    // Written to standard error directly: the test harness captures `eprintln!`
    // output of passing tests, and a skip must stay visible.
    let mut stderr = std::io::stderr().lock();
    let mut explained = EXPLAINED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !explained.contains(&requirement) {
        explained.push(requirement);
        let _ = writeln!(stderr, "{reason}");
    }
    let _ = writeln!(
        stderr,
        "skipped: {test} needs {}",
        requirement.description()
    );
}

/// An initialized runtime, or `None` when no NDI runtime library can be loaded.
///
/// A test that needs the real SDK starts with
/// `let Some(ndi) = ndi() else { return };`. Every failure other than a missing
/// runtime library panics.
pub fn ndi() -> Option<NDI> {
    match NDI::new() {
        Ok(ndi) => Some(ndi),
        Err(Error::RuntimeLoadFailed(reason)) => {
            skip(
                Requirement::Runtime,
                &format!("no NDI runtime could be loaded: {reason}"),
            );
            None
        }
        Err(error) => panic!("the NDI runtime loaded but did not initialize: {error}"),
    }
}

/// An initialized Advanced SDK runtime, or `None` when the selected runtime is
/// not one or this build lacks the Advanced SDK declarations.
pub fn advanced_ndi() -> Option<NDI> {
    let ndi = ndi()?;
    match ndi.require_feature(RuntimeFeature::AdvancedSenderConfiguration) {
        Ok(()) => Some(ndi),
        Err(error) => {
            skip(
                Requirement::Advanced,
                &format!("the Advanced SDK cannot be used: {error}"),
            );
            None
        }
    }
}
