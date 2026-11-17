//! Guards accept owner proofs, and their native owners remain borrowed.

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn captured_frames_require_their_owning_source() {
    let output = nested_cargo::command("features-advanced", "rustc", None, None)
        .args([
            "--lib",
            "--no-default-features",
            "--features",
            "advanced",
            "--",
            "--cfg",
            "ndi_test_frame_source",
        ])
        .output()
        .expect("compile frame source contract");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a runtime borrow authorized a native frame: {stderr}"
    );
    for diagnostic in ["E0277", "FrameSource", "frame_source.rs"] {
        assert!(
            stderr.contains(diagnostic),
            "missing {diagnostic}: {stderr}"
        );
    }
    if cfg!(ndi_has_advanced_av_sync) {
        assert!(
            stderr.contains("E0515"),
            "AV-sync guard escaped its owner: {stderr}"
        );
    }
}
