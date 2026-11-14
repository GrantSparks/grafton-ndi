use grafton_ndi::{Finder, SourceCache};

fn moved(finder: Finder) {
    let _cache = SourceCache::new(finder);
    let _ = finder.current_sources();
}

fn main() {}
