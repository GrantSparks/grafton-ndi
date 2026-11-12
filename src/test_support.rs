//! Support for tests that need native NDI code.
//!
//! The crate's unit tests use this module directly and its integration tests
//! include the same file, so both follow one policy:
//!
//! * a test that needs native code asks for it first and returns when the answer
//!   is `None`, so it runs wherever that code is installed and is visibly skipped
//!   elsewhere;
//! * automation that must not skip lists what its environment provides in
//!   `GRAFTON_NDI_REQUIRE_RUNTIME`, which turns the matching skips into failures;
//! * a listed entry whose tests are not part of the build fails too, through
//!   [`unverifiable_promises`]: it could otherwise never be broken.

use std::{io::Write, sync::Mutex};

use super::{
    capabilities::{self, RuntimeFeature},
    Error, Ndi,
};

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
    /// An NDI|HX video decoder the selected runtime can use, which a test that
    /// receives HEVC or H.264 video as pictures needs. Listed as `hx-decoder`.
    ///
    /// The runtime finds its decoder when it runs and has no query for it.
    /// Without one, a receiver is given black 1920x1080 UYVY frames in place of
    /// the picture. The Advanced SDK manual documents the decoder as built in
    /// on Windows and macOS. On Linux, NDI 6.4.0.2 was measured, with strace,
    /// to decode only through:
    ///
    /// * the NDI|HX Driver for Linux, `libndihx.so` with its bundled
    ///   `libavcodec-ndi.so.61` and the other `-ndi` FFmpeg 7 libraries,
    ///   copied beside `libndi.so.6` or `libndi_advanced.so.6`; or
    /// * FFmpeg 7's `libavcodec-ndi.so.61` or `libavcodec.so.61` wherever the
    ///   dynamic loader finds it.
    ///
    /// An older FFmpeg, such as the 6.x of Ubuntu 24.04, does not count.
    /// [`hx_decoder`] looks in those places.
    HxDecoder,
}

impl Requirement {
    const ALL: [Self; 5] = [
        Self::Runtime,
        Self::Advanced,
        Self::File,
        Self::DiscoveryServer,
        Self::HxDecoder,
    ];

    /// Its spelling in `GRAFTON_NDI_REQUIRE_RUNTIME`.
    fn token(self) -> &'static str {
        match self {
            Self::Runtime => "1",
            Self::Advanced => "advanced",
            Self::File => "file",
            Self::DiscoveryServer => "discovery-server",
            Self::HxDecoder => "hx-decoder",
        }
    }

    /// What a skipped test is reported to need.
    fn description(self) -> &'static str {
        match self {
            Self::Runtime => "an NDI runtime",
            Self::Advanced => "the Advanced SDK",
            Self::File => "the NDI File library",
            Self::DiscoveryServer => "the SDK's Discovery Server",
            Self::HxDecoder => "an NDI|HX video decoder",
        }
    }

    /// What this build lacks to have any test of the requirement.
    ///
    /// The tests that need native code beyond a runtime are compiled only for
    /// some features and targets. Where none of them is, a promise of that
    /// code would be kept by there being nothing to break it.
    fn needs(self) -> Option<&'static str> {
        let missing = |present: bool, needed: &'static str| (!present).then_some(needed);
        match self {
            Self::Runtime => None,
            Self::Advanced => missing(cfg!(feature = "advanced"), "the `advanced` feature"),
            Self::File => missing(cfg!(feature = "file-io"), "the `file-io` feature"),
            Self::DiscoveryServer => missing(
                cfg!(all(target_os = "linux", not(ndi_link_static))),
                "a Linux target that loads the runtime dynamically",
            ),
            // The only test that decodes NDI|HX sends it from an Advanced
            // compressed sender.
            Self::HxDecoder => missing(cfg!(feature = "advanced"), "the `advanced` feature"),
        }
    }

    /// The entries of `GRAFTON_NDI_REQUIRE_RUNTIME`.
    ///
    /// An entry that names no requirement is a configuration mistake and
    /// panics: a misspelt promise must not quietly allow skips.
    fn promised() -> Vec<Self> {
        let Some(promises) = std::env::var_os(REQUIRE_RUNTIME) else {
            return Vec::new();
        };
        let promises = promises
            .to_str()
            .unwrap_or_else(|| panic!("{REQUIRE_RUNTIME} is not UTF-8"));
        promises
            .split(',')
            .map(str::trim)
            .map(|entry| {
                Self::ALL
                    .into_iter()
                    .find(|requirement| requirement.token() == entry)
                    .unwrap_or_else(|| {
                        panic!(
                            "{REQUIRE_RUNTIME} lists {entry:?}; expected 1, advanced, file, \
                             discovery-server or hx-decoder"
                        )
                    })
            })
            .collect()
    }

    /// Whether the environment promises to provide it. Any entry promises a
    /// runtime.
    fn is_promised(self) -> bool {
        Self::promised()
            .into_iter()
            .any(|named| named == self || self == Self::Runtime)
    }
}

/// The entries of `GRAFTON_NDI_REQUIRE_RUNTIME` that no test compiled into
/// this build can check, each with the feature or target its tests need.
///
/// One always-compiled test asserts that this is empty, so that listing
/// `advanced` for a build without the `advanced` feature, for example, fails
/// instead of passing with nothing checked.
pub fn unverifiable_promises() -> Vec<String> {
    Requirement::promised()
        .into_iter()
        .filter_map(|requirement| {
            let needed = requirement.needs()?;
            Some(format!("`{}` needs {needed}", requirement.token()))
        })
        .collect()
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

/// `label` made unique to this process, for naming a sender or a router.
///
/// The NDI runtime takes a lock in the temporary directory named after each
/// sender. Two test processes that share that directory, such as two sessions
/// on one machine, would collide on a name that is the same in both.
pub fn unique_name(label: &str) -> String {
    format!("{label} {}", std::process::id())
}

/// An initialized runtime, or `None` when no NDI runtime library can be loaded.
///
/// A test that needs the real SDK starts with
/// `let Some(ndi) = ndi() else { return };`. Every failure other than a missing
/// runtime library panics.
pub fn ndi() -> Option<Ndi> {
    match Ndi::new() {
        Ok(ndi) => {
            assert_expected_identity(ndi.info());
            Some(ndi)
        }
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
pub fn advanced_ndi() -> Option<Ndi> {
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

/// Whether the selected runtime has an NDI|HX video decoder. When it has none,
/// the calling test is reported as skipped.
///
/// The runtime has no query for its decoder, so this looks where
/// [`Requirement::HxDecoder`] says the runtime does. The answer restates the
/// vendor's documentation for Windows and macOS and, on Linux, what NDI
/// 6.4.0.2 was measured with strace to open. It is not a vendor API: a runtime
/// that looks elsewhere needs this updated.
///
/// # Panics
///
/// Panics when no decoder is found and `GRAFTON_NDI_REQUIRE_RUNTIME` promises
/// `hx-decoder`.
pub fn hx_decoder(ndi: &Ndi) -> bool {
    match missing_hx_decoder(ndi) {
        None => true,
        Some(reason) => {
            skip(Requirement::HxDecoder, &reason);
            false
        }
    }
}

/// The NDI|HX decoder is part of the runtime on Windows and macOS.
#[cfg(any(windows, target_os = "macos"))]
fn missing_hx_decoder(_: &Ndi) -> Option<String> {
    None
}

/// Why the runtime has no NDI|HX decoder on Linux, or `None` when it has one.
///
/// The runtime library's search path is `$ORIGIN`, and it asks the dynamic
/// loader for each name below. So a decoder counts when the copy in the
/// directory the runtime was loaded from loads, or when the loader of this
/// process can load it by name. A copy that is present but cannot load, for
/// want of `libcurl.so.4` for example, decodes nothing. Loading runs only the
/// library's initializers, which the runtime would run too, and it is
/// unloaded again at once.
#[cfg(target_os = "linux")]
fn missing_hx_decoder(ndi: &Ndi) -> Option<String> {
    use std::{ffi::OsString, path::Path};

    /// The NDI|HX Driver, then FFmpeg 7's decoder under the name of the
    /// driver's copy and under its own.
    const DECODERS: [&str; 3] = ["libndihx.so", "libavcodec-ndi.so.61", "libavcodec.so.61"];

    let directory = ndi.info().library_path().and_then(Path::parent);
    let mut failures = Vec::new();
    for name in DECODERS {
        let beside = directory
            .map(|directory| directory.join(name))
            .filter(|path| path.is_file());
        for candidate in beside
            .map(OsString::from)
            .into_iter()
            .chain([OsString::from(name)])
        {
            // SAFETY: loading runs the library's initializers. These are the
            // libraries the NDI runtime itself loads to decode NDI|HX; nothing
            // in them is called, and the handle is closed when it is dropped.
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(_) => return None,
                // The loader's own message, which names the file it tried, is
                // the source of libloading's error.
                Err(error) => failures.push(match std::error::Error::source(&error) {
                    Some(cause) => cause.to_string(),
                    None => error.to_string(),
                }),
            }
        }
    }
    let places = directory.map_or_else(
        || "by name (a statically linked runtime has no directory)".to_owned(),
        |directory| {
            format!(
                "from beside the runtime in {} or by name",
                directory.display()
            )
        },
    );
    Some(format!(
        "the NDI runtime has no NDI|HX video decoder: none of {} loads {places} ({})",
        DECODERS.join(", "),
        failures.join("; ")
    ))
}

/// No NDI|HX decoder is documented for other targets.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn missing_hx_decoder(_: &Ndi) -> Option<String> {
    Some(format!(
        "no NDI|HX video decoder is documented for {}",
        std::env::consts::OS
    ))
}

/// Assert the identity promised by SDK provisioning against the module loaded
/// in this process. These values are separate from header selection, allowing
/// builds with newer headers to be tested against an older runtime.
pub fn assert_expected_identity(info: &capabilities::RuntimeInfo) {
    let expected =
        |name: &str| std::env::var(name).unwrap_or_else(|error| panic!("{name}: {error}"));
    if std::env::var_os("GRAFTON_NDI_EXPECTED_VERSION").is_some() {
        let actual = info
            .version()
            .expect("the selected SDK version must be recognized");
        assert_eq!(
            actual.to_string(),
            expected("GRAFTON_NDI_EXPECTED_VERSION"),
            "the loaded runtime differs from the provisioned SDK version"
        );
    }
    if std::env::var_os("GRAFTON_NDI_EXPECTED_EDITION").is_some() {
        let actual = if info.edition() == capabilities::SdkEdition::Standard {
            "standard"
        } else if info.edition() == capabilities::SdkEdition::Advanced {
            "advanced"
        } else {
            panic!("the selected SDK edition must be recognized")
        };
        assert_eq!(
            actual,
            expected("GRAFTON_NDI_EXPECTED_EDITION"),
            "the loaded runtime differs from the provisioned SDK edition"
        );
    }
    if let Some(expected) = std::env::var_os("GRAFTON_NDI_EXPECTED_RUNTIME") {
        let actual = info
            .library_path()
            .expect("the provisioned SDK is a shared runtime");
        assert_eq!(
            actual.canonicalize().expect("the loaded module exists"),
            std::path::Path::new(&expected)
                .canonicalize()
                .expect("the provisioned runtime exists"),
            "the test loaded a different module than SDK provisioning selected"
        );
    }
}
