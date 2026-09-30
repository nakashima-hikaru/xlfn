//! Allocation regression probe, separate from the uninstrumented timing benchmarks.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::benchmark_support::{
    BenchHandleObject, NumericArrayOutputBenchmark, RawArgumentIngressBenchmark,
    SemanticIdentityBenchmark, Utf16IdentityBenchmark,
};
use xlfn::output::XlArrayBuilder;

const WARMUP_CALLS: usize = 100;
const MEASURED_CALLS: usize = 10_000;
const ARRAY_CELLS: usize = 1_000;
const ARRAY_OUTPUT_CALLS: usize = 100;

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
    measure_calls(case, MEASURED_CALLS, &mut operation)
}

fn measure_calls(case: &str, calls: usize, mut operation: impl FnMut()) -> Counts {
    for _ in 0..WARMUP_CALLS {
        operation();
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    REALLOCATIONS.store(0, Ordering::Relaxed);
    REQUESTED_BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    for _ in 0..calls {
        operation();
    }
    ENABLED.store(false, Ordering::Relaxed);
    let counts = Counts {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        reallocations: REALLOCATIONS.load(Ordering::Relaxed),
        requested_bytes: REQUESTED_BYTES.load(Ordering::Relaxed),
    };
    println!("{case}: calls={calls} {counts:?}");
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
    let text = Utf16IdentityBenchmark::new(&"日本語💡".repeat(80));
    measure("identity/utf16_1k", || {
        black_box(text.run());
    });
    let matrix = SemanticIdentityBenchmark::new(
        xlfn::value::Matrix::new(1, 1_000, vec![42.0; 1_000]).unwrap(),
    );
    let single = measure("identity/matrix_1k", || {
        black_box(matrix.run());
    });
    let multiple = measure("identity/eight_matrix_1k", || {
        black_box(matrix.run_arguments(8));
    });
    assert_eq!(
        multiple, single,
        "hash workspace must be reused within a call"
    );
    for cells in [1, 1_000] {
        measure(&format!("array{cells}/one_string_rest_numbers"), || {
            let mut builder = XlArrayBuilder::new(1, cells).unwrap();
            builder.push("Ready").unwrap();
            for _ in 1..cells {
                builder.push_f64(1.0).unwrap();
            }
            black_box(builder.finish().unwrap());
        });
    }
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

    for cells in [1_000, 100_000] {
        let benchmark = NumericArrayOutputBenchmark::new(cells);
        let matrix = measure_calls(
            &format!("array_numeric_output/{cells}/matrix_build_return"),
            ARRAY_OUTPUT_CALLS,
            || benchmark.run_matrix(),
        );
        let direct = measure_calls(
            &format!("array_numeric_output/{cells}/builder_build_return"),
            ARRAY_OUTPUT_CALLS,
            || benchmark.run_direct(),
        );
        if !cfg!(feature = "refinement") {
            assert_eq!(matrix.allocations, direct.allocations + ARRAY_OUTPUT_CALLS);
            assert_eq!(
                matrix.deallocations,
                direct.deallocations + ARRAY_OUTPUT_CALLS
            );
            assert_eq!(matrix.reallocations, direct.reallocations);
            assert_eq!(
                matrix.requested_bytes,
                direct.requested_bytes + ARRAY_OUTPUT_CALLS * cells * std::mem::size_of::<f64>(),
                "direct output must avoid the intermediate numerical result buffer",
            );
        }
    }

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
