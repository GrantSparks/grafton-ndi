//! Invocation policy shared by tests that compile another Cargo graph.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Build a nested Cargo command with dependencies retained under `nested/graph`.
///
/// `target` selects the target under test when provided, or the compiler's host
/// for type-level contracts. `external_workspace` selects an unlocked consumer
/// workspace; repository builds use the repository lockfile. Caller and CI job
/// limits are inherited unchanged.
pub fn command(
    graph: &str,
    subcommand: &str,
    target: Option<&str>,
    external_workspace: Option<&Path>,
) -> Command {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target_directory = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                manifest.join(path)
            }
        })
        .unwrap_or_else(|| manifest.join("target"))
        .join("nested")
        .join(graph);
    let host;
    let target = match target {
        Some(target) => target,
        None => {
            let compiler =
                Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                    .arg("-vV")
                    .output()
                    .expect("query the compiler's host target");
            assert!(
                compiler.status.success(),
                "could not query the compiler host"
            );
            host = String::from_utf8(compiler.stdout)
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("host: "))
                .expect("rustc reports its host target")
                .to_owned();
            &host
        }
    };
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(external_workspace.unwrap_or(manifest))
        .env("CARGO_TARGET_DIR", target_directory)
        .env("CARGO_TERM_COLOR", "never")
        .args([subcommand, "--offline", "--target", target]);
    if external_workspace.is_none() {
        command.arg("--locked");
    }
    command
}
