// module: cache
use super::StoredCache;
use std::ptr::NonNull;

fn retag_key(
    pointer: NonNull<StoredCache<(), fn(&()), u64>>,
) -> NonNull<StoredCache<(), fn(&'static ()), u64>> {
    pointer // error: E0308
}

fn retag_value(
    pointer: NonNull<StoredCache<(), u64, fn(&())>>,
) -> NonNull<StoredCache<(), u64, fn(&'static ())>> {
    pointer // error: E0308
}

fn retag_marker(
    pointer: NonNull<StoredCache<fn(&()), u64, u64>>,
) -> NonNull<StoredCache<fn(&'static ()), u64, u64>> {
    pointer // error: E0308
}
