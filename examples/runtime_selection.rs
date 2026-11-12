//! Example: selecting an NDI runtime, inspecting it, then starting it.
//!
//! Selecting loads the NDI runtime library without starting NDI, so a program
//! can report which library it found, check that it can run on this CPU and
//! see which optional capabilities it has before committing to it.
//!
//! Run with: `cargo run --example runtime_selection`
//!
//! Optional argument, the absolute path of the runtime library to load instead
//! of searching for one:
//! `cargo run --example runtime_selection -- /opt/ndi/lib/libndi.so.6`

use grafton_ndi::{Error, Ndi, Runtime, RuntimeFeature, RuntimeOptions};

/// Capabilities that vary between runtime versions and editions.
const FEATURES: [RuntimeFeature; 6] = [
    RuntimeFeature::Planar444Video,
    RuntimeFeature::ReceiveDelay,
    RuntimeFeature::SdkTimestamp,
    RuntimeFeature::Routing,
    RuntimeFeature::SenderDiscovery,
    RuntimeFeature::AdvancedSenderConfiguration,
];

fn main() -> Result<(), Error> {
    let mut options = RuntimeOptions::default();
    if let Some(path) = std::env::args().nth(1) {
        options = options.library_path(path);
    }

    // Step one: select. A failure lists every location that was tried.
    let runtime = Runtime::select(&options)?;
    let info = runtime.info();
    println!("Selected NDI runtime");
    println!("  version: {}", info.raw_version());
    println!("  edition: {:?}", info.edition());
    match info.library_path() {
        Some(path) => println!("  library: {}", path.display()),
        None => println!("  library: linked into this program"),
    }
    println!(
        "  CPU:     {}",
        if runtime.is_supported_cpu() {
            "supported"
        } else {
            "not supported"
        }
    );

    println!("Optional capabilities");
    for feature in FEATURES {
        println!(
            "  {feature:?}: {:?}",
            runtime.capabilities().status(feature)
        );
    }
    println!("NDI started: {}", Ndi::is_running());

    // Step two: initialize. NDI runs for as long as a handle exists.
    let ndi = runtime.initialize()?;
    println!("NDI started: {}", Ndi::is_running());
    drop(ndi);
    Ok(())
}
