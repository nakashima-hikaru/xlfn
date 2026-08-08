use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{SyncBenchKind, SyncBoundaryWorkerPool};

const ITERATIONS_PER_THREAD: usize = 1000;

fn sync_boundary_benchmarks(c: &mut Criterion) {
    let mut group_admission = c.benchmark_group("sync_boundary/admission");
    for threads in [1_usize, 2, 4, 8, 16, 32] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group_admission.throughput(Throughput::Elements(attempts as u64));
        group_admission.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched_ref(
                    || SyncBoundaryWorkerPool::new(threads, ITERATIONS_PER_THREAD),
                    |pool| {
                        pool.run_batch(SyncBenchKind::AdmissionOnly);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group_admission.finish();

    let mut group_scalar = c.benchmark_group("sync_boundary/scalar_return");
    for threads in [1_usize, 2, 4, 8, 16, 32] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group_scalar.throughput(Throughput::Elements(attempts as u64));
        group_scalar.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched_ref(
                    || SyncBoundaryWorkerPool::new(threads, ITERATIONS_PER_THREAD),
                    |pool| {
                        pool.run_batch(SyncBenchKind::ScalarReturn);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group_scalar.finish();
}

criterion_group!(benches, sync_boundary_benchmarks);
criterion_main!(benches);
