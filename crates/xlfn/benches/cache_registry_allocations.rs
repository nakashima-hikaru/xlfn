//! Public endpoint construction and registry reclamation allocation probe.
//!
//! Endpoint descriptors, process warmup, and reporting are outside snapshots.
//! Counts cover all threads and include the registry, shared index, cache
//! metadata, and one seeded u64 value per endpoint. Requested bytes describe
//! allocator layouts, not RSS or allocator-reserved memory.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::cache::{CacheEndpoint, CacheRegistry};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REQUESTED_BYTES: AtomicUsize = AtomicUsize::new(0);
static RELEASED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: all allocation operations delegate unchanged to System. Successful
// operations update only atomic counters, which cannot allocate or reenter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies a live System allocation and its layout.
        unsafe { System.dealloc(pointer, layout) };
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        RELEASED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller supplies a live allocation and valid new size.
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(size, Ordering::Relaxed);
            RELEASED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        replacement
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct Snapshot {
    allocations: usize,
    deallocations: usize,
    reallocations: usize,
    requested_bytes: usize,
    released_bytes: usize,
}

impl Snapshot {
    fn capture() -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            reallocations: REALLOCATIONS.load(Ordering::Relaxed),
            requested_bytes: REQUESTED_BYTES.load(Ordering::Relaxed),
            released_bytes: RELEASED_BYTES.load(Ordering::Relaxed),
        }
    }

    fn live_delta_from(self, before: Self) -> i128 {
        (self.requested_bytes - before.requested_bytes) as i128
            - (self.released_bytes - before.released_bytes) as i128
    }
}

#[allow(
    clippy::large_stack_arrays,
    reason = "evaluated only at compile time to initialize static allocation-probe endpoint names"
)]
const fn endpoint_names() -> [[u8; 13]; 8192] {
    let mut names = [*b"registry-0000"; 8192];
    let mut index = 0;
    while index < names.len() {
        let mut value = index;
        let mut digit = 13;
        while digit > 9 {
            digit -= 1;
            names[index][digit] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        index += 1;
    }
    names
}

static ENDPOINT_NAMES: [[u8; 13]; 8192] = endpoint_names();

fn seed(registry: &CacheRegistry, endpoints: &[CacheEndpoint<u64, u64>]) {
    for (index, endpoint) in endpoints.iter().enumerate() {
        let value = index as u64;
        let lease = registry
            .get_or_try_insert(endpoint, 1, |_| size_of::<u64>(), || Ok(value))
            .expect("registry allocation probe seeds an endpoint");
        assert_eq!(*lease, value);
        black_box(&*lease);
    }
}

fn main() {
    let endpoints: Vec<_> = ENDPOINT_NAMES
        .iter()
        .map(|name| {
            CacheEndpoint::<u64, u64>::new(
                std::str::from_utf8(name).expect("generated endpoint names are ASCII"),
            )
        })
        .collect();

    // Initialize unavoidable process/TLS metadata without assigning its first
    // use to one endpoint count. Each measured registry is still newly built.
    let warmup = CacheRegistry::new(64);
    seed(&warmup, &endpoints[..1]);
    drop(warmup);

    for count in [1, 64, 8192] {
        for trial in ["first", "steady"] {
            let before = Snapshot::capture();
            let registry = CacheRegistry::new(64);
            seed(&registry, &endpoints[..count]);
            black_box(&registry);
            let retained = Snapshot::capture();
            drop(registry);
            let after_drop = Snapshot::capture();

            // Constructing JSON and stdout buffers must not enter either
            // construction or reclamation snapshots.
            println!(
                "{}",
                serde_json::json!({
                    "probe": "cache_registry_construction_and_drop",
                    "endpoints": count,
                    "trial": trial,
                    "process_warmup_endpoints": 1,
                    "allocations": retained.allocations - before.allocations,
                    "deallocations_during_construction": retained.deallocations - before.deallocations,
                    "reallocations": retained.reallocations - before.reallocations,
                    "requested_bytes": retained.requested_bytes - before.requested_bytes,
                    "released_bytes_during_construction": retained.released_bytes - before.released_bytes,
                    "retained_requested_bytes": retained.live_delta_from(before),
                    "drop_allocations": after_drop.allocations - retained.allocations,
                    "drop_deallocations": after_drop.deallocations - retained.deallocations,
                    "drop_reallocations": after_drop.reallocations - retained.reallocations,
                    "drop_requested_bytes": after_drop.requested_bytes - retained.requested_bytes,
                    "drop_released_bytes": after_drop.released_bytes - retained.released_bytes,
                    "after_drop_live_requested_bytes": after_drop.live_delta_from(before),
                })
            );
        }
    }
}
