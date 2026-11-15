//! Compile the private finder guard contract in its real module context.

#[test]
fn a_foreign_mutex_cannot_supply_the_finder_snapshot_guard() {
    use std::{path::PathBuf, process::Command};

    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"))
        .join("internal-finder-snapshot-guard");
    let features = [
        ("advanced", cfg!(feature = "advanced")),
        ("file-io", cfg!(feature = "file-io")),
        ("image-encoding", cfg!(feature = "image-encoding")),
        ("tokio", cfg!(feature = "tokio")),
        ("smol", cfg!(feature = "smol")),
    ]
    .into_iter()
    .filter_map(|(feature, enabled)| enabled.then_some(feature))
    .collect::<Vec<_>>()
    .join(",");

    let compile = |foreign: bool| {
        let mut command = Command::new(env!("CARGO"));
        command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_JOBS", "3")
            .env("CARGO_TERM_COLOR", "never")
            .args([
                "rustc",
                "--offline",
                "--locked",
                "--lib",
                "--no-default-features",
            ]);
        if !features.is_empty() {
            command.args(["--features", &features]);
        }
        command.args(["--", "--cfg", "ndi_test_finder_snapshot_guard"]);
        if foreign {
            command.args(["--cfg", "ndi_test_foreign_snapshot_guard"]);
        }
        command
            .output()
            .expect("compile the private finder snapshot guard fixture")
    };

    let accepted = compile(false);
    assert!(
        accepted.status.success(),
        "a finder-issued guard must compile against the actual kernel:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let refused = compile(true);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    let compact: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !refused.status.success(),
        "a foreign MutexGuard<()> constructed a finder snapshot guard"
    );
    assert!(
        stderr.contains("E0308")
            && stderr.contains("FindInstance")
            && stderr.contains("MutexGuard")
            && compact.contains(",()>"),
        "the foreign guard must fail the protected-handle type, not another contract:\n{stderr}"
    );
    assert!(
        !stderr.contains("E0603") && !stderr.contains("E0616"),
        "a privacy failure does not prove the guard contract:\n{stderr}"
    );
}
