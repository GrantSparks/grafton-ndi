//! The real validated-frame constructor refuses unchecked or foreign parts.
#[test]
fn frame_parts_require_the_matching_descriptor_validator() {
    use std::{path::PathBuf, process::Command};
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
        .join("internal-descriptor-parts");
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_JOBS", "4")
        .env("CARGO_TERM_COLOR", "never")
        .args([
            "rustc",
            "--offline",
            "--locked",
            "--lib",
            "--no-default-features",
            "--",
            "--cfg",
            "ndi_test_descriptor_parts",
        ])
        .output()
        .expect("compile the actual validated-frame parts fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "unchecked frame parts compiled");
    for expected in [
        "UnvalidatedParts",
        "DescriptorParts",
        "NDIlib_video_frame_v2_t",
        "NDIlib_audio_frame_v3_t",
        "E0277",
    ] {
        assert!(
            stderr.contains(expected),
            "missing {expected} bound diagnostic:\n{stderr}"
        );
    }
    assert!(
        !stderr.contains("E0603") && !stderr.contains("E0616"),
        "privacy must not be the reason the fixture fails:\n{stderr}"
    );
}
