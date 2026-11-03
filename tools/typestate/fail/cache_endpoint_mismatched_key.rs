// module: cache
use super::{CacheEndpoint, CacheRegistry, StoredCache, endpoint_cache};
use std::ptr::NonNull;

fn mismatched_key(
    registry: &CacheRegistry,
    endpoint: &CacheEndpoint<u64, String>,
    pointer: NonNull<StoredCache<(), u32, String>>,
) {
    endpoint_cache::remember(registry.identity, endpoint, pointer); // error: E0308
}
