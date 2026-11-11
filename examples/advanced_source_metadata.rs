//! Example: discovering sources together with the metadata they advertise.
//!
//! This example demonstrates:
//! - Creating a finder with an Advanced SDK configuration
//! - Listing each source with its metadata
//!
//! A sender advertises metadata in the `send.metadata` property of its own
//! configuration, and a finder is told it by a Discovery Server: name the
//! server in the `networks.discovery` property of the configuration below.
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_source_metadata`

use std::time::Duration;

use grafton_ndi::{advanced::AdvancedConfig, Error, Finder, FinderOptions, NDI};

fn main() -> Result<(), Error> {
    let ndi = NDI::new()?;
    // The properties of this JSON document are defined by the NDI Advanced SDK.
    let config = AdvancedConfig::parse("{}")?;
    let finder = Finder::new_with_config(
        &ndi,
        &FinderOptions::builder().show_local_sources(true).build(),
        &config,
    )?;

    println!("Searching for NDI sources...");
    finder.wait_for_sources(Duration::from_secs(5))?;
    let sources = finder.sources_with_metadata()?;
    if sources.is_empty() {
        println!("No NDI sources found!");
    }
    for source in &sources {
        println!("{}", source.source());
        match source.metadata() {
            Some(metadata) => println!("  metadata: {metadata}"),
            None => println!("  no metadata"),
        }
    }
    Ok(())
}
