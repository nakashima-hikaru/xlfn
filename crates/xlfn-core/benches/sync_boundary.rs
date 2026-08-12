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
        (
            SyncBenchKind::IngressUdfPreheld,
            "sync_boundary/ingress_udf_preheld",
        ),
        (
            SyncBenchKind::RuntimeEnterOnly,
            "sync_boundary/runtime_enter_only",
        ),
        (SyncBenchKind::FullAdmission, "sync_boundary/admission"),
        (
            SyncBenchKind::ActiveUdfSnapshot,
            "sync_boundary/active_udf_snapshot",
        ),
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

    for (kind, name) in [
        (
            SyncBenchKind::ScalarReturnNoSubscriber,
            "scalar_return/no_subscriber",
        ),
        (
            SyncBenchKind::ScalarReturnSubscriberTargetDisabled,
            "scalar_return/subscriber_target_disabled",
        ),
        (
            SyncBenchKind::ScalarReturnUdfTraceEnabled,
            "scalar_return/udf_trace_enabled",
        ),
        (
            SyncBenchKind::ScalarReturnCustomLayer,
            "scalar_return/custom_layer",
        ),
    ] {
        let mut group_scalar = c.benchmark_group(format!("sync_boundary/{name}"));
        group_scalar.measurement_time(Duration::from_secs(10));
        for threads in [1_usize, 2, 4, 8, 16, 32] {
            let attempts = threads * ITERATIONS_PER_THREAD;
            group_scalar.throughput(Throughput::Elements(attempts as u64));
            group_scalar.bench_with_input(
                BenchmarkId::from_parameter(threads),
                &threads,
                |b, &threads| {
                    let pool = SyncBoundaryWorkerPool::new(threads, ITERATIONS_PER_THREAD, kind);
                    b.iter(|| pool.run_batch());
                },
            );
        }
        group_scalar.finish();
    }
}

criterion_group!(benches, sync_boundary_benchmarks);
criterion_main!(benches);
