// module: cache
use super::{CacheEndpoint, CacheRegistry, endpoint_cache};
use std::ptr::NonNull;

fn erased_pair(
    registry: &CacheRegistry,
    endpoint: &CacheEndpoint<u64, String>,
    pointer: NonNull<()>,
) {
    endpoint_cache::remember(registry.identity, endpoint.key(), pointer); // error: E0308
}
