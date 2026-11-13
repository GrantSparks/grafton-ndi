use grafton_ndi::{Error, Ndi, Source, SourceCache};
use std::time::Duration;

fn cache_contract<T: Default + Send + Sync + 'static>() {}

fn main() {
    // The cache holds no runtime handle: it exists before one does.
    cache_contract::<SourceCache>();
    let _: fn() -> SourceCache = SourceCache::new;

    // A lookup that may search the network is given the handle to search with.
    let _: fn(&SourceCache, &Ndi, &str, Duration) -> Result<Source, Error> =
        SourceCache::find_by_host;

    // Everything else is the cache alone.
    let _: fn(&SourceCache, &str) = SourceCache::invalidate;
    let _: fn(&SourceCache) = SourceCache::clear;
    let _: fn(&SourceCache) -> usize = SourceCache::len;
    let _: fn(&SourceCache) -> bool = SourceCache::is_empty;
}
