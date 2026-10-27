// module: cache
use super::CacheEndpoint;

fn retag_key(
    endpoint: CacheEndpoint<fn(&()), u64>,
) -> CacheEndpoint<fn(&'static ()), u64> {
    endpoint // error: E0308
}

fn retag_value(
    endpoint: CacheEndpoint<u64, fn(&())>,
) -> CacheEndpoint<u64, fn(&'static ())> {
    endpoint // error: E0308
}

fn retag_marker(
    endpoint: CacheEndpoint<u64, u64, fn(&())>,
) -> CacheEndpoint<u64, u64, fn(&'static ())> {
    endpoint // error: E0308
}
