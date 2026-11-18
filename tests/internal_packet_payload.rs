//! The checked extent cannot be detached from its private packet allocation.
#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn packet_payload_fields_are_private_to_their_storage_module() {
    for (cfg, code, names) in [
        (
            "ndi_test_packet_payload",
            "E0451",
            &["PacketBytes", "PacketSlice", "CompressedFrame"][..],
        ),
        (
            "ndi_test_packet_payload_mutation",
            "E0616",
            &["len", "PacketBytes"][..],
        ),
    ] {
        let output = nested_cargo::command("features-advanced", "rustc", None, None)
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
            .expect("compile the internal packet payload fixture");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "foreign packet payload compiled");
        for expected in std::iter::once(code).chain(names.iter().copied()) {
            assert!(stderr.contains(expected), "missing {expected}:\n{stderr}");
        }
        for diagnostic in stderr.lines().filter(|line| line.starts_with("error[")) {
            assert!(
                diagnostic.starts_with(&format!("error[{code}]")),
                "unrelated error: {diagnostic}\n{stderr}"
            );
        }
    }
}
