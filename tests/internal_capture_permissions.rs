//! Verify crate-internal adapters cannot capture slots reserved by FrameSync.

#[test]
fn attached_modes_fail_the_internal_capture_entry_points() {
    use std::{path::PathBuf, process::Command};

    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
        .join("internal-capture-permissions");
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_TERM_COLOR", "never")
        .args([
            "rustc",
            "--offline",
            "--locked",
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
