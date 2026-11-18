//! Compile the ledger timeout contract at its actual module boundary.
#![cfg(feature = "file-io")]

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn a_timeout_requires_the_settlement_kernels_late_drain_proof() {
    let compile = |without_proof: bool| {
        let mut command = nested_cargo::command("features-file-io", "rustc", None, None);
        command.args([
            "--lib",
            "--no-default-features",
            "--features",
            "file-io",
            "--",
            "--cfg",
            "ndi_test_file_late_drain",
        ]);
        if without_proof {
            command.args(["--cfg", "ndi_test_file_late_drain_without_proof"]);
        }
        command
            .output()
            .expect("compile the ledger late-drain fixture")
    };
    let accepted = compile(false);
    assert!(
        accepted.status.success(),
        "accepted fixture:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let refused = compile(true);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "a boolean authorized a timeout");
    assert!(
        stderr.contains("E0308") && stderr.contains("LateDrain") && stderr.contains("bool"),
        "wrong refusal:\n{stderr}"
    );
    assert!(
        !stderr.contains("E0603") && !stderr.contains("E0616"),
        "a privacy error does not establish the proof parameter:\n{stderr}"
    );
}

#[test]
fn native_stop_and_late_drain_proofs_cannot_be_forged_or_discarded() {
    let compile = |cfg| {
        nested_cargo::command("features-file-io", "rustc", None, None)
            .args([
                "--lib",
                "--no-default-features",
                "--features",
                "file-io",
                "--",
                "--cfg",
                cfg,
                "-D",
                "warnings",
            ])
            .output()
            .expect("compile the recorder proof fixture")
    };
    let forged = compile("ndi_test_file_proofs");
    let stderr = String::from_utf8_lossy(&forged.stderr);
    assert!(
        !forged.status.success()
            && stderr.matches("E0451").count() >= 2
            && stderr.contains("StopOwed")
            && stderr.contains("LateDrain"),
        "{stderr}"
    );

    let discarded = compile("ndi_test_file_proofs_discarded");
    let stderr = String::from_utf8_lossy(&discarded.stderr);
    assert!(
        !discarded.status.success()
            && stderr.contains("unused_must_use")
            && stderr.contains("a session transition may owe a native stop"),
        "{stderr}"
    );
}
