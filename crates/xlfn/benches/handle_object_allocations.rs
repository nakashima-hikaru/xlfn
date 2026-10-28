//! Allocation count and requested live bytes for real object/alias storage.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::benchmark_support::{ObjectPayloadKind, ObjectStorageBenchmark, object_storage_layout};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;
// SAFETY: all layouts, sizes and pointers pass unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            REQUESTED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: this live allocation and layout came from this allocator.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller supplies a live allocation and valid new size.
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            LIVE.fetch_add(size, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            REQUESTED.fetch_add(size, Ordering::Relaxed);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct Snapshot {
    live: usize,
    allocations: usize,
    reallocations: usize,
    requested: usize,
}

fn snapshot() -> Snapshot {
    Snapshot {
        live: LIVE.load(Ordering::Relaxed),
        allocations: ALLOCS.load(Ordering::Relaxed),
        reallocations: REALLOCS.load(Ordering::Relaxed),
        requested: REQUESTED.load(Ordering::Relaxed),
    }
}

fn delta(before: Snapshot, after: Snapshot) -> serde_json::Value {
    serde_json::json!({
        "allocations": after.allocations - before.allocations,
        "reallocations": after.reallocations - before.reallocations,
        "requested_bytes": after.requested - before.requested,
        "live_bytes_delta": after.live as i128 - before.live as i128,
    })
}

fn main() {
    // Warm the same retirement/TLS paths before counting. JSON and printing
    // occur only after all measured snapshots have been collected.
    for aliases in [1, 32] {
        let fixture = ObjectStorageBenchmark::new(256, aliases);
        fixture.run_cycle(ObjectPayloadKind::U64);
    }
    println!("{}", serde_json::json!({"layout": object_storage_layout()}));
    for objects in [1, 256] {
        for aliases in [1, 32] {
            for kind in ObjectPayloadKind::ALL {
                let before = snapshot();
                let fixture = ObjectStorageBenchmark::new(objects, aliases);
                let empty = snapshot();
                fixture.populate(kind);
                let populated = snapshot();
                fixture.clear();
                let cleared = snapshot();
                fixture.populate(kind);
                let warm = snapshot();
                fixture.clear();
                drop(fixture);
                let dropped = snapshot();
                assert_eq!(
                    dropped.live, before.live,
                    "fixture must release all owned storage"
                );
                println!(
                    "{}",
                    serde_json::json!({
                        "case": kind.name(), "objects": objects, "aliases_per_object": aliases,
                        "empty": delta(before, empty),
                        "cold_population": delta(empty, populated),
                        "cold_retained_bytes": populated.live - before.live,
                        "cleared_retained_bytes": cleared.live - before.live,
                        "warm_population": delta(cleared, warm),
                        "warm_retained_bytes": warm.live - before.live,
                        "after_drop_live_delta": dropped.live as i128 - before.live as i128,
                    })
                );
            }
        }
    }
}
