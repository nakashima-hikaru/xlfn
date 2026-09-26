//! Allocation regression probe, separate from the uninstrumented timing benchmarks.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{BenchHandleObject, RawArgumentIngressBenchmark};
use xlfn::output::XlArrayBuilder;

const WARMUP_CALLS: usize = 100;
const MEASURED_CALLS: usize = 10_000;
const ARRAY_CELLS: usize = 1_000;

struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REQUESTED_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: allocation/deallocation and layouts are forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: the allocator contract supplies a valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if ENABLED.load(Ordering::Relaxed) {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: pointer and layout came from the same delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED_BYTES.fetch_add(size, Ordering::Relaxed);
        }
        // SAFETY: the caller supplies a live allocation and a valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    allocations: usize,
    deallocations: usize,
    reallocations: usize,
    requested_bytes: usize,
}

fn measure(case: &str, mut operation: impl FnMut()) -> Counts {
    for _ in 0..WARMUP_CALLS {
        operation();
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    REALLOCATIONS.store(0, Ordering::Relaxed);
    REQUESTED_BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    for _ in 0..MEASURED_CALLS {
        operation();
    }
    ENABLED.store(false, Ordering::Relaxed);
    let counts = Counts {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        reallocations: REALLOCATIONS.load(Ordering::Relaxed),
        requested_bytes: REQUESTED_BYTES.load(Ordering::Relaxed),
    };
    println!("{case}: calls={MEASURED_CALLS} {counts:?}");
    counts
}

fn assert_runtime_allocation_free(case: &str, counts: Counts) {
    if cfg!(feature = "refinement") {
        println!("{case}: zero-allocation assertion skipped with refinement tracing enabled");
    } else {
        assert_eq!(counts, Counts::default(), "{case} must not allocate");
    }
}

#[derive(Clone, Copy, xlfn::ExcelEnum)]
#[excel_enum(crate = "xlfn")]
enum Status {
    Ready,
}

fn main() {
    let strings = measure("array1000/str", || {
        let mut builder = XlArrayBuilder::new(1, ARRAY_CELLS).unwrap();
        for _ in 0..ARRAY_CELLS {
            builder.push(black_box("Ready")).unwrap();
        }
        black_box(builder.finish().unwrap());
    });
    let enums = measure("array1000/enum", || {
        let mut builder = XlArrayBuilder::new(1, ARRAY_CELLS).unwrap();
        for _ in 0..ARRAY_CELLS {
            builder.push(black_box(Status::Ready)).unwrap();
        }
        black_box(builder.finish().unwrap());
    });
    assert_eq!(
        enums, strings,
        "enum labels must use the borrowed string path"
    );

    let mut numeric = RawArgumentIngressBenchmark::number(42.0);
    let numbers = measure("f64/plain", || numeric.run_plain::<f64>());
    assert_runtime_allocation_free("f64/plain", numbers);

    let mut handle = RawArgumentIngressBenchmark::handle();
    let plain = measure("handle/plain", || {
        handle.run_handle_plain::<BenchHandleObject>()
    });
    let identity = measure("handle/with_identity", || {
        black_box(handle.run_handle_with_identity::<BenchHandleObject>());
    });
    assert_runtime_allocation_free("handle/plain", plain);
    assert_runtime_allocation_free("handle/with_identity", identity);

    #[cfg(feature = "async")]
    {
        let pending = measure("handle/async_pending", || {
            handle.run_handle_pending::<BenchHandleObject>()
        });
        assert_runtime_allocation_free("handle/async_pending", pending);
    }
}
