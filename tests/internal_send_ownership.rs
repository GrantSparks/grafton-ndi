//! Compile the actual owner, slot and queue modules at their privacy boundary.
#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn native_loans_and_slot_keys_require_their_owner() {
    let compile = |cfg| {
        nested_cargo::command("features-advanced", "rustc", None, None)
            .args([
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
            &[
                "flush", "handle", "clone", "claim", "E0382", "E0599", "E0616", "instance",
            ][..],
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
