//! Native tables borrow an initialized runtime unless retained by a binding.
#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn native_tables_cannot_escape_their_runtime_owner() {
    let compile = |cfg| {
        nested_cargo::command("runtime-binding", "rustc", None, None)
            .args(["--lib", "--no-default-features", "--", "--cfg", cfg])
            .output()
            .expect("compile the runtime binding fixture")
    };
    let control = compile("ndi_test_runtime_binding_control");
    assert!(
        control.status.success(),
        "accepted fixture: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    let rejected = compile("ndi_test_runtime_binding");
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        !rejected.status.success(),
        "unbound static native tables compiled"
    );
    for diagnostic in [
        "escaping_api",
        "escaping_optional",
        "escaping_bound",
        "lifetime may not live long enough",
    ] {
        assert!(
            stderr.contains(diagnostic),
            "missing {diagnostic}:\n{stderr}"
        );
    }
    let privacy = compile("ndi_test_runtime_binding_privacy");
    let stderr = String::from_utf8_lossy(&privacy.stderr);
    assert!(
        !privacy.status.success(),
        "foreign module forged a runtime binding"
    );
    assert!(
        stderr.contains("E0451"),
        "missing private binding fields: {stderr}"
    );
}
