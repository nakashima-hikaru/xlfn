use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
use xlfn::benchmark_support::{TwoPhaseBenchmark, TwoPhaseInputKind as Kind};

struct Counting;
static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
// SAFETY: every allocation operation is forwarded unchanged to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size(), Relaxed);
        }
        // SAFETY: forward the caller-provided layout unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forward the allocation and its original layout unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if TRACK.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size, Relaxed);
        }
        // SAFETY: forward the live allocation and requested size unchanged.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn main() {
    for (kind, size, label) in [
        (Kind::Number, 1, "f64"),
        (Kind::Numbers, 1000, "matrix_f64_1k"),
        (Kind::Numbers, 100_000, "matrix_f64_100k"),
        (Kind::Strings, 10_000, "matrix_string_10k"),
    ] {
        for phase in ["warm", "cold", "changed_last"] {
            for (round, candidate) in [false, true, true, false].into_iter().enumerate() {
                let mut fixture = TwoPhaseBenchmark::new(kind, size);
                fixture.run(candidate).unwrap();
                let iterations = match size {
                    1 => 100_000,
                    1000 => 10_000,
                    10_000 => 300,
                    _ => 200,
                };
                let mut elapsed = 0;
                for _ in 0..iterations {
                    if phase != "warm" {
                        fixture.reset();
                    }
                    if phase == "changed_last" {
                        fixture.run(candidate).unwrap();
                        fixture.change_last();
                    }
                    let start = Instant::now();
                    std::hint::black_box(fixture.run(candidate).unwrap());
                    elapsed += start.elapsed().as_nanos();
                }
                if phase != "warm" {
                    fixture.reset();
                }
                if phase == "changed_last" {
                    fixture.run(candidate).unwrap();
                    fixture.change_last();
                }
                ALLOCS.store(0, Relaxed);
                BYTES.store(0, Relaxed);
                TRACK.store(true, Relaxed);
                let result = fixture.run(candidate).unwrap();
                TRACK.store(false, Relaxed);
                std::hint::black_box(result);
                println!(
                    "{}",
                    serde_json::json!({"case": label, "phase": phase, "candidate": candidate, "round": round, "ns": elapsed / iterations, "allocations": ALLOCS.load(Relaxed), "allocated_bytes": BYTES.load(Relaxed), "factory_calls": fixture.factory_calls, "materializations": fixture.materializations})
                );
            }
        }
    }
}
