//! Compile the actual owner, slot and queue modules at their privacy boundary.
#[test]
fn native_loans_and_slot_keys_require_their_owner() {
    use std::{path::PathBuf, process::Command};
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
        .join("internal-send-ownership");
    let compile = |cfg| {
        Command::new(env!("CARGO"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_JOBS", "1")
            .env("CARGO_TERM_COLOR", "never")
            .args([
                "rustc",
                "--offline",
                "--locked",
                "--lib",
                "--no-default-features",
                "--features",
                "advanced",
                "--",
                "--cfg",
                cfg,
            ])
            .output()
            .expect("compile the internal send ownership fixture")
    };
    let control = compile("ndi_test_send_ownership_control");
    assert!(
        control.status.success(),
        "accepted fixture: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    for (cfg, diagnostics) in [
        (
            "ndi_test_send_ownership",
            &[
                "fields `slot` and `sender`",
                "fields `sender` and `loans`",
                "Destroyed",
                "Completion",
                "E0451",
            ][..],
        ),
        (
            "ndi_test_send_ownership_types",
            &["flush", "handle", "clone", "claim", "E0382", "E0599"][..],
        ),
    ] {
        let rejected = compile(cfg);
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(!rejected.status.success(), "foreign native loans compiled");
        for diagnostic in diagnostics {
            assert!(
                stderr.contains(diagnostic),
                "missing {diagnostic}:\n{stderr}"
            );
        }
    }
}
