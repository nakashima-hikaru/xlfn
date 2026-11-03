// module: cache
use super::{CacheEndpoint, CacheRegistry, StoredCache, endpoint_cache};
use std::ptr::NonNull;

fn remember_same_type(
    registry: &CacheRegistry,
    endpoint: &CacheEndpoint<u64, String>,
    pointer: NonNull<StoredCache<(), u64, String>>,
) {
    endpoint_cache::remember(registry.identity, endpoint, pointer);
    let _: Option<NonNull<StoredCache<(), u64, String>>> =
        endpoint_cache::lookup(registry.identity, endpoint);
}

fn resolve_same_type(registry: &CacheRegistry, endpoint: &CacheEndpoint<u64, String>) {
    let _: crate::XllResult<NonNull<StoredCache<(), u64, String>>> =
        registry.resolve_cache_uncached(endpoint);
}
