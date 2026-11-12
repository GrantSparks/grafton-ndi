//! An old or partial Advanced SDK must compile the same public Rust API.
// Full bindgen/compiler integration runs where the Advanced SDK job already
// provides libclang. SDK-free builds exercise the selector in build_configuration.
#![cfg(all(feature = "advanced", ndi_has_advanced_header))]

#[path = "support/fixture.rs"]
mod fixture;

#[test]
fn partial_advanced_headers_disable_only_the_missing_declarations() {
    let fixture = fixture::FixtureDir::create("partial-advanced");
    let include = fixture.path().join("include");
    std::fs::create_dir(&include).unwrap();
    // Synthetic ABI declarations, not redistributed vendor headers. The
    // baseline types are imported from the vendored Standard bindings, just
    // as they are with a real installation.
    std::fs::write(
        include.join("Processing.NDI.Lib.h"),
        r#"
        // Permission is hereby granted, free of charge
        typedef struct NDIlib_send_create_t NDIlib_send_create_t;
        typedef struct NDIlib_send_instance_type* NDIlib_send_instance_t;
        typedef struct NDIlib_recv_create_v3_t NDIlib_recv_create_v3_t;
        typedef struct NDIlib_recv_instance_type* NDIlib_recv_instance_t;
    "#,
    )
    .unwrap();
    for functions in [
        r#"NDIlib_send_instance_t NDIlib_send_create_v2(const NDIlib_send_create_t*, const char*);"#,
        r#"NDIlib_recv_instance_t NDIlib_recv_create_v4(const NDIlib_recv_create_v3_t*, const char*);"#,
        "",
    ] {
        std::fs::write(
            include.join("Processing.NDI.Advanced.h"),
            format!("#include <Processing.NDI.Lib.h>\n{functions}\n"),
        )
        .unwrap();
        let output = std::process::Command::new(env!("CARGO"))
            .args([
                "test",
                "--locked",
                "--offline",
                "--lib",
                "--all-features",
                "capabilities::tests",
            ])
            .env("NDI_SDK_DIR", fixture.path())
            .env("RUSTFLAGS", "-D warnings")
            .env("CARGO_TARGET_DIR", fixture.path().join("target"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "partial Advanced SDK failed to compile: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
