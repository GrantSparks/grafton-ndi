//! Support for tests that need native NDI code.
//!
//! Unit tests select the helpers they use here. Integration tests include the
//! same focused source modules through `tests/common/mod.rs`, so prerequisite
//! skips and runtime identity checks follow one policy.

use crate as api;

#[cfg(feature = "advanced")]
mod advanced_runtime;
mod child;
mod feature;
#[cfg(feature = "file-io")]
mod file_api;
mod fixture;
mod hx_placeholder;
mod identity;
mod policy;
mod runtime;
#[cfg(feature = "advanced")]
mod stop;
mod unique_name;
mod unsupported;

#[cfg(feature = "advanced")]
pub use advanced_runtime::advanced_ndi;
pub use child::TestChild;
pub use feature::require_feature;
#[cfg(feature = "file-io")]
pub use file_api::file_api;
pub use fixture::FixtureDir;
pub use hx_placeholder::is_hx_placeholder;
use policy::Requirement;
pub use runtime::ndi;
#[cfg(feature = "advanced")]
pub use stop::StopOnDrop;
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
        Requirement::Peer => missing(
            cfg!(not(ndi_link_static)),
            "a target that loads the runtime dynamically",
        ),
        Requirement::DiscoveryServer => missing(
            cfg!(all(target_os = "linux", not(ndi_link_static))),
            "a Linux target that loads the runtime dynamically",
        ),
        // Their Advanced prerequisite is checked through the implication
        // table, just as an explicit `advanced` token is.
        Requirement::H264Decoder | Requirement::Hevc444Decoder => None,
    }
}

/// Payload size may change only the numeric extents of bounded summaries.
pub(crate) fn assert_bounded_debug(small: &[String], large: &[String]) {
    fn without_counts(text: &str) -> String {
        let mut previous_digit = false;
        text.chars()
            .filter_map(|character| {
                let digit = character.is_ascii_digit();
                let emit = !digit || !previous_digit;
                previous_digit = digit;
                emit.then_some(if digit { '#' } else { character })
            })
            .collect()
    }
    assert_eq!(small.len(), large.len());
    for (small, large) in small.iter().zip(large) {
        for text in [small, large] {
            assert!(
                text.len() < 1024,
                "unbounded media Debug: {} bytes",
                text.len()
            );
            assert!(
                !text.contains("0x"),
                "media Debug contains an address: {text}"
            );
        }
        assert_ne!(small, large, "media Debug omitted its payload extent");
        assert_eq!(without_counts(small), without_counts(large));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, CapabilityRefusal, Error, RuntimeFeature, SdkVersion};
    use std::sync::LazyLock;

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
        for requirement in Requirement::ALL {
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
    fn concurrent_fixtures_with_the_same_purpose_own_distinct_paths() {
        let fixtures: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| FixtureDir::create("concurrent-test")))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect()
        });
        let paths: std::collections::HashSet<_> = fixtures.iter().map(FixtureDir::path).collect();
        assert_eq!(paths.len(), fixtures.len());
        assert!(paths.iter().all(|path| path.is_dir()));
    }

    #[test]
    fn fixture_cleans_up_after_unwinding() {
        let mut path = None;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fixture = FixtureDir::create("unwinding-test");
            path = Some(fixture.path().to_owned());
            std::fs::write(fixture.join("sentinel"), b"test").unwrap();
            panic!("test fixture scope unwound");
        }));
        assert!(outcome.is_err());
        assert!(!path.unwrap().exists());
    }

    #[test]
    fn child_skips_are_forwarded_to_the_diagnostic_writer() {
        const CHILD: &str = "GRAFTON_CHILD_SKIP_FIXTURE";
        const REASON: &str = "the skip fixture has no Discovery Server";
        if std::env::var_os(CHILD).is_some() {
            policy::skip(Requirement::DiscoveryServer, REASON);
            return;
        }

        let output = TestChild::new(
            "test_support::tests::child_skips_are_forwarded_to_the_diagnostic_writer",
        )
        .env(CHILD, "1")
        .env_remove("GRAFTON_NDI_REQUIRE_RUNTIME")
        .output(true);
        let mut diagnostics = Vec::new();
        child::forward_skips(&output, &mut diagnostics);
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        let mut lines = diagnostics.lines();
        assert_eq!(lines.next(), Some(REASON));
        assert!(lines.next().unwrap().starts_with(
            "skipped: test_support::tests::child_skips_are_forwarded_to_the_diagnostic_writer"
        ));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn peer_promises_require_a_dynamic_build_and_a_second_runtime() {
        const CHILD: &str = "GRAFTON_PEER_PROMISE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(Requirement::Peer.is_promised());
            assert!(Requirement::Runtime.is_promised());
            assert_eq!(
                unverifiable_promises().is_empty(),
                cfg!(not(ndi_link_static))
            );
            policy::skip(Requirement::Peer, "the peer fixture has no second runtime");
            return;
        }

        let mut child = TestChild::new(
            "test_support::tests::peer_promises_require_a_dynamic_build_and_a_second_runtime",
        );
        child
            .env(CHILD, "1")
            .env("GRAFTON_NDI_REQUIRE_RUNTIME", "peer");
        let output = child.output(false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("promises peer but the peer fixture has no second runtime"),
            "{stderr}"
        );

        child.env("GRAFTON_NDI_REQUIRE_RUNTIME", "peer,unknown");
        let output = child.output(false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("lists \"unknown\""), "{stderr}");
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
            TestChild::new(
                "test_support::tests::decoder_promises_enforce_their_implied_requirements",
            )
            .env(CHILD, "1")
            .env("GRAFTON_NDI_REQUIRE_RUNTIME", tokens)
            .run();
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
            let mut command = TestChild::new("test_support::tests::feature_refusals_distinguish_prerequisites_from_runtime_limits");
            command.env(CHILD, "1");
            if let Some(promise) = promise {
                command.env("GRAFTON_NDI_REQUIRE_RUNTIME", promise);
            } else {
                command.env_remove("GRAFTON_NDI_REQUIRE_RUNTIME");
            }
            let output = command.run();
            let stderr = String::from_utf8_lossy(&output.stderr);
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
