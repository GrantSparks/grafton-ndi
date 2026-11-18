//! Native receiver identities can escape their owner only through permissions.

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn receiver_handles_and_input_dispatch_require_their_proofs() {
    let output = nested_cargo::command("features-advanced", "rustc", None, None)
        .args([
            "--lib",
            "--no-default-features",
            "--features",
            "advanced",
            "--",
            "--cfg",
            "ndi_test_receiver_proofs",
        ])
        .output()
        .expect("compile internal receiver permission fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "unguarded receiver identity escaped"
    );
    for diagnostic in [
        "E0616",
        "field `instance`",
        "field `0`",
        "E0308",
        "&MutatingControl",
        "E0599",
        "no method named `set_tally`",
        "no method named `send_metadata`",
        "no method named `ptz_zoom`",
        "no method named `add_metadata`",
        "no method named `kvm_key_press`",
        "no method named `request_keyframe`",
    ] {
        assert!(
            stderr.contains(diagnostic),
            "missing {diagnostic}:\n{stderr}"
        );
    }
}
