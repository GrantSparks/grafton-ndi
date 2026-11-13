use grafton_ndi::{Finder, SourceCache};

fn escaping(finder: Finder) -> SourceCache<'static> {
    SourceCache::new(&finder)
}

fn main() {}
