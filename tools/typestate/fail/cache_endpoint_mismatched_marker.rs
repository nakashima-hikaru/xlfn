// module: cache
use super::{CacheEndpoint, CacheRegistry, StoredCache, endpoint_cache};
use std::ptr::NonNull;

enum First {}
enum Second {}

fn mismatched_marker(
    registry: &CacheRegistry,
    endpoint: &CacheEndpoint<u64, String, First>,
    pointer: NonNull<StoredCache<Second, u64, String>>,
) {
    endpoint_cache::remember(registry.identity, endpoint, pointer); // error: E0308
}
