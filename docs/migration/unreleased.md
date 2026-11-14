# Migrate from 3.0 to the unreleased API

## Source cache ownership

Move the configured finder into the cache:

```diff
-let cache = SourceCache::new(&finder);
+let cache = SourceCache::new(finder);
```

Replace `SourceCache<'a>` field and return types with `SourceCache`. The cache
owns the finder and retains its runtime; it can be returned from a constructor,
stored directly in server state and shared through `Arc<SourceCache>`.

Use `cache.finder()` wherever subsequent code previously used `finder` for
uncached discovery or selection. This returns `&Finder`, preserving its original
configuration and snapshot synchronization. Cache construction moves the finder
without an allocation, reference count or SDK call. There is no borrowed cache
variant or deprecated constructor.

Host matching, full-window discovery, timeout budgets, copy limits and
invalidation semantics are unchanged. See the current
[server-state pattern](../usage.md#keep-discovery-in-server-state).
