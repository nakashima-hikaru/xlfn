//! Standalone CacheNode payload-layout experiment; not a Cargo bench target.
//! See cache-node-layout.md for reproduction, scope, and measured limitations.
//!
//! Layout mirrors CacheNode metadata at the time of the experiment. Keep the
//! two alternatives identical except for Box<V> versus directly stored V.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::mem::{align_of, size_of};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

struct Tracked;
static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);
// SAFETY: every allocation operation forwards the original layout and pointer
// to System. The counters never access allocation contents.
unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forward the layout required by GlobalAlloc::alloc.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK.load(Ordering::Relaxed) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forward the pointer/layout required by GlobalAlloc::dealloc.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Tracked = Tracked;

// Identical metadata fields/order to the current CacheNode. The pointee type
// of domain does not affect its pointer layout; no domain is dereferenced.
#[allow(
    dead_code,
    reason = "Mirror the cache node metadata without running its admission protocol"
)]
struct BoxedNode<V> {
    value: Box<V>,
    pins: AtomicUsize,
    resident: AtomicBool,
    weight: u32,
    generation: u64,
    domain: NonNull<()>,
}
#[allow(
    dead_code,
    reason = "Mirror the cache node metadata without running its admission protocol"
)]
struct DirectNode<V> {
    value: V,
    pins: AtomicUsize,
    resident: AtomicBool,
    weight: u32,
    generation: u64,
    domain: NonNull<()>,
}

fn boxed<V>(value: V) -> Box<BoxedNode<V>> {
    Box::new(BoxedNode {
        value: Box::new(value),
        pins: AtomicUsize::new(2),
        resident: AtomicBool::new(true),
        weight: 1,
        generation: 1,
        domain: NonNull::dangling(),
    })
}
fn direct<V>(value: V) -> Box<DirectNode<V>> {
    Box::new(DirectNode {
        value,
        pins: AtomicUsize::new(2),
        resident: AtomicBool::new(true),
        weight: 1,
        generation: 1,
        domain: NonNull::dangling(),
    })
}
fn drop_boxed<V>(node: Box<BoxedNode<V>>) -> bool {
    let value = node.value;
    catch_unwind(AssertUnwindSafe(|| drop(value))).is_err()
}
fn drop_direct<V>(node: Box<DirectNode<V>>) -> bool {
    // Keep large values in their node allocation instead of moving them to
    // an unwind closure. Box drop glue deallocates during unwinding too.
    catch_unwind(AssertUnwindSafe(|| drop(node))).is_err()
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_size(ptr: *const std::ffi::c_void) -> usize;
}

#[allow(
    clippy::borrowed_box,
    reason = "The Box proves this is a live allocation from the global System-backed allocator"
)]
fn allocator_size<T>(allocation: &Box<T>) -> Option<usize> {
    if size_of::<T>() == 0 {
        return Some(0);
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the borrowed Box retains a live, nonzero allocation from
        // System, whose macOS backend is compatible with malloc_size.
        Some(unsafe { malloc_size((&**allocation as *const T).cast()) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = allocation;
        None
    }
}

fn sizes<V>(name: &str, value: impl Fn() -> V) {
    let b = boxed(value());
    let d = direct(value());
    let boxed_real = allocator_size(&b)
        .zip(allocator_size(&b.value))
        .map(|(node, payload)| node + payload);
    let direct_real = allocator_size(&d);
    let format_size = |bytes: Option<usize>| {
        bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "unavailable".to_owned())
    };
    println!(
        "layout,{name},value={},align={},boxed_requested={},direct_requested={},boxed_allocator={},direct_allocator={}",
        size_of::<V>(),
        align_of::<V>(),
        size_of::<BoxedNode<V>>() + size_of::<V>(),
        size_of::<DirectNode<V>>(),
        format_size(boxed_real),
        format_size(direct_real)
    );
}

fn time<V>(name: &str, iterations: usize, value: impl Fn() -> V) {
    let mut boxed_times = Vec::new();
    let mut direct_times = Vec::new();
    for round in 0..9 {
        for variant in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let started = Instant::now();
            if variant {
                for _ in 0..iterations {
                    black_box(drop_direct(black_box(direct(black_box(value())))));
                }
            } else {
                for _ in 0..iterations {
                    black_box(drop_boxed(black_box(boxed(black_box(value())))));
                }
            }
            let nanos = started.elapsed().as_nanos() as f64 / iterations as f64;
            if round > 0 {
                if variant {
                    direct_times.push(nanos);
                } else {
                    boxed_times.push(nanos);
                }
            }
        }
    }
    boxed_times.sort_by(|a, b| a.total_cmp(b));
    direct_times.sort_by(|a, b| a.total_cmp(b));
    println!(
        "latency,{name},iterations={iterations},boxed_median_ns={:.2},direct_median_ns={:.2},boxed_max_ns={:.2},direct_max_ns={:.2}",
        (boxed_times[3] + boxed_times[4]) / 2.0,
        (direct_times[3] + direct_times[4]) / 2.0,
        boxed_times[7],
        direct_times[7]
    );
}

fn counts<V>(name: &str, value: impl Fn() -> V) {
    for variant in [false, true] {
        ALLOCS.store(0, Ordering::Relaxed);
        FREES.store(0, Ordering::Relaxed);
        TRACK.store(true, Ordering::Relaxed);
        for _ in 0..1024 {
            if variant {
                black_box(drop_direct(black_box(direct(value()))));
            } else {
                black_box(drop_boxed(black_box(boxed(value()))));
            }
        }
        TRACK.store(false, Ordering::Relaxed);
        println!(
            "allocation,{name},direct={variant},allocs={},frees={}",
            ALLOCS.load(Ordering::Relaxed),
            FREES.load(Ordering::Relaxed)
        );
    }
}

#[repr(align(64))]
#[allow(
    dead_code,
    reason = "Payload alignment is the subject of the layout probe"
)]
struct Aligned([u8; 64]);
struct Panics {
    _bytes: [u8; 8192],
}
impl Drop for Panics {
    fn drop(&mut self) {
        panic!("drop probe");
    }
}
fn main() {
    sizes("u64", || 42u64);
    sizes("vec8k", || vec![7u8; 8192]);
    sizes("array8k", || [7u8; 8192]);
    sizes("array64k", || [7u8; 65536]);
    sizes("aligned64", || Aligned([7; 64]));
    sizes("zst", || ());
    counts("u64", || 42u64);
    counts("array8k", || [7u8; 8192]);
    time("u64", 500_000, || 42u64);
    time("vec8k", 50_000, || vec![7u8; 8192]);
    time("array8k", 50_000, || [7u8; 8192]);
    time("array64k", 10_000, || [7u8; 65536]);
    std::panic::set_hook(Box::new(|_| {}));
    // Warm up panic runtime allocations so counters cover the drop itself.
    assert!(catch_unwind(|| panic!("warmup")).is_err());
    for variant in [false, true] {
        ALLOCS.store(0, Ordering::Relaxed);
        FREES.store(0, Ordering::Relaxed);
        TRACK.store(true, Ordering::Relaxed);
        let panicked = if variant {
            drop_direct(direct(Panics { _bytes: [0; 8192] }))
        } else {
            drop_boxed(boxed(Panics { _bytes: [0; 8192] }))
        };
        TRACK.store(false, Ordering::Relaxed);
        let allocations = ALLOCS.load(Ordering::Relaxed);
        let frees = FREES.load(Ordering::Relaxed);
        println!("panic,direct={variant},caught={panicked},allocs={allocations},frees={frees}");
        assert!(panicked);
        assert_eq!(allocations, frees);
    }
}
