//! The real validated-frame constructor refuses unchecked or foreign parts.
#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn frame_parts_require_the_matching_descriptor_validator() {
    let output = nested_cargo::command("no-default-features", "rustc", None, None)
        .args([
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
