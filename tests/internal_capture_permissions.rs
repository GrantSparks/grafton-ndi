//! Verify crate-internal adapters cannot capture slots reserved by FrameSync.

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn attached_modes_fail_the_internal_capture_entry_points() {
    let output = nested_cargo::command("no-default-features", "rustc", None, None)
        .args([
            "--lib",
            "--no-default-features",
            "--",
            "--cfg",
            "ndi_test_capture_permissions",
        ])
        .output()
        .expect("compile the internal capture permission fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostics = stderr.replace("capture::", "").replace("receiver::", "");
    assert!(
        !output.status.success(),
        "an attached receiver captured a reserved media/event slot"
    );
    for permission in ["AudioKind", "AllSlots", "NoSlots"] {
        assert!(
            diagnostics.contains(&format!("FrameSyncAttached: Permits<{permission}>")),
            "the fixture did not fail the {permission} permission bound:\n{stderr}"
        );
    }
    assert!(
        stderr.contains("capture_ref_until") && stderr.contains("capture_audio"),
        "internal and public typed capture must both fail their permission gates:\n{stderr}"
    );
    assert!(
        stderr.contains("no method named `detach`"),
        "raw recovery must require the attached synchronizer owner:\n{stderr}"
    );
}
