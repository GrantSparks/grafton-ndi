use grafton_ndi::{Finder, Result, Source, SourceCache};
use std::time::Duration;

fn cache_contract<T: Send + Sync>() {}

fn configured(finder: &Finder) -> Result<()> {
    let cache = SourceCache::new(finder);
    let _: Vec<Source> = cache.sources_by_host("studio.local", Duration::ZERO)?;
    cache.invalidate("STUDIO.LOCAL.");
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
    let _: Option<Source> = finder.find_source_by_name("STUDIO (Camera 2)", Duration::ZERO)?;
    let _: Option<Source> = finder.find_source(Duration::ZERO, |source| {
        source.name() == "STUDIO (Camera 2)" && source.matches_host("192.0.2.1")
    })?;
    Ok(())
}

fn main() {
    cache_contract::<SourceCache<'static>>();
}
