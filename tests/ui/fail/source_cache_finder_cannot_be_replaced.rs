use grafton_ndi::{Finder, SourceCache};

fn replace(cache: &mut SourceCache, finder: Finder) {
    *cache.finder() = finder;
}

fn main() {}
