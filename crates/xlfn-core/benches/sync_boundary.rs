use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{SyncBenchKind, SyncBoundaryWorkerPool};

const ITERATIONS_PER_THREAD: usize = 1000;

fn sync_boundary_benchmarks(c: &mut Criterion) {
    for (kind, name) in [
        (
            SyncBenchKind::IngressUdfOnly,
            "sync_boundary/ingress_udf_only",
        ),
        (SyncBenchKind::FullAdmission, "sync_boundary/admission"),
    ] {
        let mut group_admission = c.benchmark_group(name);
        group_admission.measurement_time(Duration::from_secs(10));
        for threads in [1_usize, 2, 4, 8, 16, 32] {
            let attempts = threads * ITERATIONS_PER_THREAD;
            group_admission.throughput(Throughput::Elements(attempts as u64));
            group_admission.bench_with_input(
                BenchmarkId::from_parameter(threads),
                &threads,
                |b, &threads| {
                    let pool = SyncBoundaryWorkerPool::new(threads, ITERATIONS_PER_THREAD, kind);
                    b.iter(|| pool.run_batch());
                },
            );
        }
        group_admission.finish();
    }

    let mut group_no_sub = c.benchmark_group("sync_boundary/scalar_return/no_subscriber");
    group_no_sub.measurement_time(Duration::from_secs(10));
    for threads in [1_usize, 2, 4, 8, 16, 32] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group_no_sub.throughput(Throughput::Elements(attempts as u64));
        group_no_sub.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                let pool = SyncBoundaryWorkerPool::new(
                    threads,
                    ITERATIONS_PER_THREAD,
                    SyncBenchKind::ScalarReturnNoSubscriber,
                );
                b.iter(|| pool.run_batch());
            },
        );
    }
    group_no_sub.finish();

    let mut group_trace = c.benchmark_group("sync_boundary/scalar_return/udf_trace_enabled");
    group_trace.measurement_time(Duration::from_secs(10));
    for threads in [1_usize] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group_trace.throughput(Throughput::Elements(attempts as u64));
        group_trace.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                let pool = SyncBoundaryWorkerPool::new(
                    threads,
                    ITERATIONS_PER_THREAD,
                    SyncBenchKind::ScalarReturnUdfTraceEnabled,
                );
                b.iter(|| pool.run_batch());
            },
        );
    }
    group_trace.finish();
}

criterion_group!(benches, sync_boundary_benchmarks);
criterion_main!(benches);
