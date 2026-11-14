//! Support for tests that need native NDI code.
//!
//! Unit tests select the helpers they use here. Integration tests include the
//! same focused source modules through `tests/common/mod.rs`, so prerequisite
//! skips and runtime identity checks follow one policy.

use crate as api;

#[cfg(feature = "advanced")]
mod advanced_runtime;
#[cfg(feature = "file-io")]
mod file_api;
mod hx_placeholder;
mod identity;
mod policy;
mod runtime;
mod unique_name;
#[cfg(feature = "file-io")]
mod unsupported;

#[cfg(feature = "advanced")]
pub use advanced_runtime::advanced_ndi;
#[cfg(feature = "file-io")]
pub use file_api::file_api;
pub use hx_placeholder::is_hx_placeholder;
use policy::Requirement;
pub use runtime::ndi;
pub use unique_name::unique_name;
#[cfg(feature = "file-io")]
pub use unsupported::unsupported;

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
            let needed = missing_requirement(requirement)?;
            Some(format!("`{}` needs {needed}", requirement.token()))
        })
        .collect()
}

/// What this build lacks to have any test of the requirement.
///
/// The tests that need native code beyond a runtime are compiled only for
/// some features and targets. Where none of them is, a promise of that
/// code would be kept by there being nothing to break it.
fn missing_requirement(requirement: Requirement) -> Option<&'static str> {
    let missing = |present: bool, needed: &'static str| (!present).then_some(needed);
    match requirement {
        Requirement::Runtime => None,
        Requirement::Advanced => missing(cfg!(feature = "advanced"), "the `advanced` feature"),
        Requirement::File => missing(cfg!(feature = "file-io"), "the `file-io` feature"),
        Requirement::DiscoveryServer => missing(
            cfg!(all(target_os = "linux", not(ndi_link_static))),
            "a Linux target that loads the runtime dynamically",
        ),
        // The tests that decode NDI|HX send it from an Advanced compressed
        // sender.
        Requirement::H264Decoder | Requirement::Hevc444Decoder => {
            missing(cfg!(feature = "advanced"), "the `advanced` feature")
        }
    }
}
