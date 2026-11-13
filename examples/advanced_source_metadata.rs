//! Example: discovering sources together with the metadata they advertise.
//!
//! This example demonstrates:
//! - Creating a finder with an Advanced SDK configuration
//! - Listing each source with its metadata
//!
//! A sender advertises metadata in the `send.metadata` property of its own
//! configuration, and a finder is told it by a Discovery Server. The
//! configuration below is empty, so the finder reports metadata only when a
//! Discovery Server is configured elsewhere, for example in the machine's NDI
//! configuration. To name one here, set the `networks.discovery` property.
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_source_metadata`

use std::time::Duration;

use grafton_ndi::{advanced::AdvancedConfig, Error, Finder, FinderOptions, Ndi};

fn main() -> Result<(), Error> {
    let ndi = Ndi::new()?;
    // The properties of this JSON document are defined by the NDI Advanced SDK.
    let config = AdvancedConfig::parse("{}")?;
    let finder = Finder::new(&ndi, &FinderOptions::new().config(config))?;

    println!("Searching for NDI sources...");
    // Allow the full discovery window for late sources before taking metadata.
    finder.find_sources(Duration::from_secs(5))?;
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
