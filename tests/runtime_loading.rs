//! Loader tests run each selection scenario in its own process: successful module
//! selection is intentionally irreversible, while failed selection is retryable.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture_cc.rs"]
mod fixture_cc;

use grafton_ndi::{
    capabilities::{RuntimeFeature, SdkEdition},
    runtime::RuntimeOptions,
    Error, NDI,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn compile_fixture(
    directory: &Path,
    name: &str,
    omit_required: bool,
    dependency: Option<&Path>,
) -> PathBuf {
    let sdk = std::env::var_os("NDI_SDK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/NDI SDK for Linux"));
    let output = directory.join(name);
    let mut compiler = fixture_cc::command();
    compiler
        .args(["-std=c11", "-shared", "-fPIC"])
        .arg("-I")
        .arg(sdk.join("include"))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/fake_sdk.c"
        ))
        .arg("-o")
        .arg(&output);
    if omit_required {
        compiler.arg("-DFAKE_OMIT_REQUIRED");
    }
    if let Some(dependency) = dependency {
        compiler
            .arg("-Wl,--no-as-needed")
            .arg(dependency)
            .arg("-Wl,-rpath,$ORIGIN");
    }
    let result = compiler.output().expect("run native fixture C compiler");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

#[test]
fn selected_runtime_loader_contract() {
    if let Ok(scenario) = std::env::var("GRAFTON_LOADER_CHILD") {
        child(&scenario);
        return;
    }
    let directory = std::env::temp_dir().join(format!("grafton-ndi-loader-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let complete = compile_fixture(&directory, "libndi_complete.so", false, None);
    let missing = compile_fixture(&directory, "libndi_missing.so", true, None);
    let old_advanced = directory.join("libndi_advanced.so.6");
    fs::copy(&complete, &old_advanced).unwrap();
    // A dependency may export a missing entry point. Handle-based lookup alone
    // would accept it; the loader must verify the symbol's defining module too.
    let source = directory.join("dependency.c");
    fs::write(&source, "void NDIlib_send_set_failover(void *instance, const void *source) {(void)instance; (void)source;}\n").unwrap();
    let dependency = directory.join("libndi_dependency.so");
    let result = fixture_cc::command()
        .args(["-shared", "-fPIC"])
        .arg(source)
        .arg("-o")
        .arg(&dependency)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let foreign = compile_fixture(&directory, "libndi_foreign.so", true, Some(&dependency));
    for scenario in [
        "retry_and_capabilities",
        "conflict",
        "edition",
        "process_lifetime",
        "no_imports",
        "foreign_symbol",
        "concurrent_selection",
        "old_advanced_identity",
    ] {
        let log = directory.join(format!("{scenario}.log"));
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "selected_runtime_loader_contract", "--nocapture"])
            .env("GRAFTON_LOADER_CHILD", scenario)
            .env("GRAFTON_LOADER_COMPLETE", &complete)
            .env("GRAFTON_LOADER_MISSING", &missing)
            .env("GRAFTON_LOADER_FOREIGN", &foreign)
            .env("GRAFTON_LOADER_OLD_ADVANCED", &old_advanced)
            .env("GRAFTON_FAKE_NDI_LOG", &log)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

fn child(scenario: &str) {
    let complete = PathBuf::from(std::env::var_os("GRAFTON_LOADER_COMPLETE").unwrap());
    let missing = PathBuf::from(std::env::var_os("GRAFTON_LOADER_MISSING").unwrap());
    let options = RuntimeOptions::default()
        .library_path(&complete)
        .edition(SdkEdition::Standard);
    match scenario {
        "retry_and_capabilities" => {
            assert!(matches!(
                NDI::with_options(
                    &RuntimeOptions::default()
                        .library_path(&missing)
                        .edition(SdkEdition::Standard)
                ),
                Err(Error::RuntimeLoadFailed(_))
            ));
            assert!(!NDI::is_running());
            let ndi = NDI::with_options(&options).unwrap();
            assert!(NDI::is_running());
            assert_eq!(ndi.info().library_path(), Some(complete.as_path()));
            assert_eq!(ndi.info().version().unwrap().minor(), 3);
            assert!(!ndi.capabilities().supports(RuntimeFeature::ReceiveDelay));
            assert!(!ndi.capabilities().supports(RuntimeFeature::Planar444Video));
            assert!(NDI::try_is_supported_cpu().unwrap());
            assert!(NDI::version().unwrap().contains("6.3.2.0"));
            drop(ndi);
            assert!(!NDI::is_running());
            drop(NDI::with_options(&options).unwrap());
            let log =
                fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap();
            assert_eq!(log, "initialize\ndestroy\ninitialize\ndestroy\n");
        }
        "conflict" => {
            let ndi = NDI::with_options(&options).unwrap();
            assert!(matches!(
                NDI::with_options(
                    &RuntimeOptions::default()
                        .library_path(&missing)
                        .edition(SdkEdition::Standard)
                ),
                Err(Error::RuntimeConflict(_))
            ));
            assert!(matches!(
                NDI::with_options(&RuntimeOptions::default().edition(SdkEdition::Advanced)),
                Err(Error::RuntimeConflict(_))
            ));
            drop(ndi);
        }
        "edition" => {
            // Caller assertion cannot turn Standard ABI into an Advanced runtime.
            assert!(matches!(
                NDI::with_options(&options.clone().edition(SdkEdition::Advanced)),
                Err(Error::RuntimeConflict(_))
            ));
            drop(NDI::with_options(&options).unwrap());
        }
        "process_lifetime" => {
            let ndi = NDI::with_options(&options).unwrap();
            let token = ndi.process_lifetime();
            assert_eq!(token.ndi().info(), ndi.info());
            drop(ndi);
            assert!(NDI::is_running());
            let log =
                fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap();
            assert_eq!(log, "initialize\n");
        }
        "old_advanced_identity" => {
            let library = PathBuf::from(std::env::var_os("GRAFTON_LOADER_OLD_ADVANCED").unwrap());
            let ndi = NDI::with_options(
                &RuntimeOptions::default()
                    .library_path(library)
                    .edition(SdkEdition::Advanced),
            )
            .unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Advanced);
            assert!(!ndi.info().edition_verified());
            assert!(NDI::is_running());
            assert!(!ndi
                .capabilities()
                .supports(RuntimeFeature::AdvancedSenderConfiguration));
        }
        "foreign_symbol" => {
            let foreign = PathBuf::from(std::env::var_os("GRAFTON_LOADER_FOREIGN").unwrap());
            assert!(matches!(
                NDI::with_options(&options.clone().library_path(foreign)),
                Err(Error::RuntimeConflict(_))
            ));
            drop(NDI::with_options(&options).unwrap());
        }
        "concurrent_selection" => {
            use std::sync::{Arc, Barrier};
            let ready = Arc::new(Barrier::new(17));
            let release = Arc::new(Barrier::new(17));
            let threads: Vec<_> = (0..16)
                .map(|_| {
                    let options = options.clone();
                    let ready = ready.clone();
                    let release = release.clone();
                    std::thread::spawn(move || {
                        let ndi = NDI::with_options(&options).unwrap();
                        ready.wait();
                        release.wait();
                        drop(ndi);
                    })
                })
                .collect();
            ready.wait();
            let log =
                fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap();
            assert_eq!(log, "initialize\n");
            release.wait();
            for thread in threads {
                thread.join().unwrap();
            }
            assert!(!NDI::is_running());
        }
        "no_imports" => {
            let output = Command::new("readelf")
                .args(["--wide", "--dyn-syms"])
                .arg(std::env::current_exe().unwrap())
                .output()
                .unwrap();
            assert!(output.status.success());
            let symbols = String::from_utf8(output.stdout).unwrap();
            assert!(
                !symbols
                    .lines()
                    .any(|line| line.contains(" UND ") && line.contains("NDIlib_")),
                "mandatory NDI symbol import: {symbols}"
            );
        }
        _ => panic!("unknown loader scenario"),
    }
}
