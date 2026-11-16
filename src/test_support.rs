//! Support for tests that need native NDI code.
//!
//! Unit tests select the helpers they use here. Integration tests include the
//! same focused source modules through `tests/common/mod.rs`, so prerequisite
//! skips and runtime identity checks follow one policy.

use crate as api;

#[cfg(feature = "advanced")]
mod advanced_runtime;
mod feature;
#[cfg(feature = "file-io")]
mod file_api;
mod hx_placeholder;
mod identity;
mod policy;
mod runtime;
mod unique_name;
mod unsupported;

#[cfg(feature = "advanced")]
pub use advanced_runtime::advanced_ndi;
pub use feature::require_feature;
#[cfg(feature = "file-io")]
pub use file_api::file_api;
pub use hx_placeholder::is_hx_placeholder;
use policy::Requirement;
pub use runtime::ndi;
pub use unique_name::unique_name;
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
            let needed = std::iter::once(requirement)
                .chain(requirement.implied().iter().copied())
                .find_map(missing_requirement)?;
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
        // Their Advanced prerequisite is checked through the implication
        // table, just as an explicit `advanced` token is.
        Requirement::H264Decoder | Requirement::Hevc444Decoder => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, CapabilityRefusal, Error, RuntimeFeature, SdkVersion};
    use std::sync::LazyLock;

    const REQUIREMENTS: [Requirement; 6] = [
        Requirement::Runtime,
        Requirement::Advanced,
        Requirement::File,
        Requirement::DiscoveryServer,
        Requirement::H264Decoder,
        Requirement::Hevc444Decoder,
    ];

    #[test]
    fn requirement_implications_include_decoder_prerequisites_without_cycles() {
        fn visit(requirement: Requirement, path: &mut Vec<Requirement>) {
            assert!(!path.contains(&requirement), "implication cycle: {path:?}");
            path.push(requirement);
            for &implied in requirement.implied() {
                visit(implied, path);
            }
            path.pop();
        }
        for requirement in REQUIREMENTS {
            visit(requirement, &mut Vec::new());
            if requirement != Requirement::Runtime {
                assert!(requirement.implied().contains(&Requirement::Runtime));
            }
        }
        for decoder in [Requirement::H264Decoder, Requirement::Hevc444Decoder] {
            assert!(decoder.implied().contains(&Requirement::Advanced));
        }
    }

    #[test]
    fn decoder_promises_enforce_their_implied_requirements() {
        const CHILD: &str = "GRAFTON_DECODER_PROMISE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(Requirement::Runtime.is_promised());
            assert!(Requirement::Advanced.is_promised());
            assert!(
                Requirement::H264Decoder.is_promised() || Requirement::Hevc444Decoder.is_promised()
            );
            let failures = unverifiable_promises();
            assert_eq!(
                failures.is_empty(),
                cfg!(feature = "advanced"),
                "{failures:?}"
            );
            // This is the same gate used when advanced_ndi cannot use the
            // selected runtime or this build lacks Advanced declarations.
            assert!(std::panic::catch_unwind(|| {
                policy::skip(Requirement::Advanced, "the runtime is Standard")
            })
            .is_err());
            return;
        }
        for tokens in [
            "h264-decoder",
            "hevc-444-decoder",
            "file,h264-decoder,hevc-444-decoder",
        ] {
            let tokens = if cfg!(feature = "file-io") || !tokens.starts_with("file,") {
                tokens
            } else {
                "h264-decoder,hevc-444-decoder"
            };
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "test_support::tests::decoder_promises_enforce_their_implied_requirements",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("GRAFTON_NDI_REQUIRE_RUNTIME", tokens)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{tokens}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn feature_refusals_distinguish_prerequisites_from_runtime_limits() {
        const CHILD: &str = "GRAFTON_FEATURE_REFUSAL_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(require_feature(Ok(())));
            for refusal in [
                CapabilityRefusal::BuildAbiUnavailable(c"NDIlib_test"),
                CapabilityRefusal::WrongEdition(None),
            ] {
                let result = std::panic::catch_unwind(|| {
                    require_feature(Err(Error::UnsupportedCapability {
                        capability: Capability::Runtime(RuntimeFeature::ConnectedSourceName),
                        refusal,
                    }))
                });
                if Requirement::Advanced.is_promised() {
                    assert!(result.is_err());
                } else {
                    assert!(!result.unwrap());
                }
            }
            static REQUIRED: LazyLock<SdkVersion> =
                LazyLock::new(|| SdkVersion::parse("6.4.0").unwrap());
            assert!(!require_feature(Err(Error::UnsupportedCapability {
                capability: Capability::Runtime(RuntimeFeature::ReceiveVideoOnly),
                refusal: CapabilityRefusal::RuntimeTooOld {
                    required: &REQUIRED,
                },
            })));
            return;
        }
        for promise in [None, Some("advanced")] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "test_support::tests::feature_refusals_distinguish_prerequisites_from_runtime_limits",
                "--nocapture",
            ]).env(CHILD, "1");
            if let Some(promise) = promise {
                command.env("GRAFTON_NDI_REQUIRE_RUNTIME", promise);
            } else {
                command.env_remove("GRAFTON_NDI_REQUIRE_RUNTIME");
            }
            let output = command.output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stderr}");
            assert!(
                stderr.contains("skipped:") && stderr.contains("video-only receiver bandwidth"),
                "{stderr}"
            );
            assert_eq!(
                stderr.contains("promises advanced"),
                promise.is_some(),
                "{stderr}"
            );
        }
    }
}
