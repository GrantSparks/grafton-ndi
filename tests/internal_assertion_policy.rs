//! Verify Clippy enforces the library's release assertion policy.

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn release_assertions_are_refused_but_compile_time_checks_are_allowed() {
    let check = |extra_cfg: &[&str]| {
        nested_cargo::command("no-default-features", "clippy", None, None)
            .args([
                "--lib",
                "--no-default-features",
                "--",
                "--cfg",
                "ndi_test_assertion_policy",
            ])
            .args(extra_cfg)
            .output()
            .expect("run Clippy on the library assertion fixture")
    };
    let compile_time = check(&[]);
    assert!(
        compile_time.status.success(),
        "compile-time assertion was refused:\n{}",
        String::from_utf8_lossy(&compile_time.stderr)
    );

    let release = check(&["--cfg", "ndi_test_release_assertion"]);
    let diagnostics = String::from_utf8_lossy(&release.stderr);
    assert!(!release.status.success(), "the release assertion passed");
    assert!(
        diagnostics.contains("use of a disallowed macro `std::assert`"),
        "the release assertion must fail Clippy's macro lint:\n{diagnostics}"
    );
}
