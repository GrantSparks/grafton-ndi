//! The prerequisite policy shared by unit and integration tests.

use std::{io::Write, sync::Mutex};

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
    /// An NDI|HX decoder the selected runtime can use for H.264 4:2:0 video,
    /// which a test that receives such video as pictures needs. Listed as
    /// `h264-decoder`. See [`Requirement::Hevc444Decoder`] for how the runtime
    /// finds a decoder.
    H264Decoder,
    /// An NDI|HX decoder the selected runtime can use for HEVC 4:4:4 video,
    /// which a test that receives such video as pictures needs. Listed as
    /// `hevc-444-decoder`.
    ///
    /// The runtime finds its decoder when it runs and has no query for it.
    /// Without one, a receiver is given black 1920x1080 frames in place of
    /// the picture (UYVY on Linux); [`is_hx_placeholder`] recognizes them. The
    /// Advanced SDK manual documents H.264 decoding on Windows and macOS
    /// without plugins, but H.265 only in 4:2:0 and where decoding hardware
    /// supports it. The macOS runtime decodes HEVC 4:4:4 on the hosted macOS
    /// runners; on the hosted Windows Server 2025 runners, which have no GPU,
    /// the runtime delivers the black frames for it. On Linux, NDI 6.4.0.2
    /// decodes either codec only through:
    ///
    /// * the NDI|HX Driver for Linux, `libndihx.so` with its bundled
    ///   `libavcodec-ndi.so.61` and the other `-ndi` FFmpeg 7 libraries,
    ///   copied beside `libndi.so.6` or `libndi_advanced.so.6`; or
    /// * FFmpeg 7's `libavcodec-ndi.so.61` or `libavcodec.so.61` wherever the
    ///   dynamic loader finds it.
    ///
    /// An older FFmpeg, such as the 6.x of Ubuntu 24.04, does not count.
    /// [`hx_decoder`] looks in those places.
    Hevc444Decoder,
}

impl Requirement {
    const ALL: [Self; 6] = [
        Self::Runtime,
        Self::Advanced,
        Self::File,
        Self::DiscoveryServer,
        Self::H264Decoder,
        Self::Hevc444Decoder,
    ];

    /// Its spelling in `GRAFTON_NDI_REQUIRE_RUNTIME`.
    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Runtime => "1",
            Self::Advanced => "advanced",
            Self::File => "file",
            Self::DiscoveryServer => "discovery-server",
            Self::H264Decoder => "h264-decoder",
            Self::Hevc444Decoder => "hevc-444-decoder",
        }
    }

    /// What a skipped test is reported to need.
    fn description(self) -> &'static str {
        match self {
            Self::Runtime => "an NDI runtime",
            Self::Advanced => "the Advanced SDK",
            Self::File => "the NDI File library",
            Self::DiscoveryServer => "the SDK's Discovery Server",
            Self::H264Decoder => "an NDI|HX decoder for H.264",
            Self::Hevc444Decoder => "an NDI|HX decoder for HEVC 4:4:4",
        }
    }

    /// The entries of `GRAFTON_NDI_REQUIRE_RUNTIME`.
    ///
    /// An entry that names no requirement is a configuration mistake and
    /// panics: a misspelt promise must not quietly allow skips.
    pub(super) fn promised() -> Vec<Self> {
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
                             discovery-server, h264-decoder or hevc-444-decoder"
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
