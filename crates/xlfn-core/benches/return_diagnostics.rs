use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{SyncBenchKind, SyncBoundaryWorkerPool};

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

fn return_diagnostics(c: &mut Criterion) {
    bench_kind(
        c,
        SyncBenchKind::ReturnStripeOnly,
        "return_diagnostics/return_stripe_only",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnBlockLocal,
        "return_diagnostics/return_block_local",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnEncodeScalarOnly,
        "return_diagnostics/return_encode_scalar_only",
    );
    bench_kind(
        c,
        SyncBenchKind::ReturnBoxOnly,
        "return_diagnostics/return_box_only",
    );
}

criterion_group!(benches, return_diagnostics);
criterion_main!(benches);
