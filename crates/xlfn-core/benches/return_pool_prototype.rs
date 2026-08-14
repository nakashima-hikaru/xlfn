//! Diagnostic-only comparison for a runtime-local striped ReturnBlock pool.
//!
//! This target does not alter the Excel-owned production allocation path. The
//! pool is intentionally opt-in so its result can be compared before any
//! ownership or allocator strategy is promoted to production.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{
    SyncBenchKind, SyncBoundaryWorkerPool, return_block_size_bytes,
};

const ITERATIONS_PER_THREAD: usize = 1_000;
const THREAD_COUNTS: [usize; 6] = [1, 2, 4, 8, 16, 32];

fn bench_kind(c: &mut Criterion, kind: SyncBenchKind, name: &'static str) {
    let mut group = c.benchmark_group(name);
    group.measurement_time(Duration::from_secs(10));

    for threads in THREAD_COUNTS {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group.throughput(Throughput::Elements(attempts as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                let pool = SyncBoundaryWorkerPool::new(threads, ITERATIONS_PER_THREAD, kind);
                b.iter(|| pool.run_batch());
            },
        );
    }

    group.finish();
}

fn return_pool_prototype(c: &mut Criterion) {
    eprintln!(
        "ReturnBlock size: {} bytes; XLOPER12 size: {} bytes",
        return_block_size_bytes(),
        std::mem::size_of::<xlfn_sys::XLOPER12>()
    );

    bench_kind(
        c,
        SyncBenchKind::ReturnPoolOnly,
        "return_pool_prototype/striped_pool_only",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnPoolBlockLocal,
        "return_pool_prototype/striped_pool_block_local",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnTlsOnly,
        "return_pool_prototype/tls_slot_only",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnTlsBlockLocal,
        "return_pool_prototype/tls_slot_block_local",
    );
}

criterion_group!(benches, return_pool_prototype);
criterion_main!(benches);
