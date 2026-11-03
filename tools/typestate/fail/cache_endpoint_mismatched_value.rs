// module: cache
use super::{CacheEndpoint, CacheRegistry, StoredCache, endpoint_cache};
use std::ptr::NonNull;

fn mismatched_value(
    registry: &CacheRegistry,
    endpoint: &CacheEndpoint<u64, String>,
    pointer: NonNull<StoredCache<(), u64, u32>>,
) {
    endpoint_cache::remember(registry.identity, endpoint, pointer); // error: E0308
}
