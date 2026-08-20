use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{ConcurrentHandleResolutionBenchmark, MultiHandleCallBenchmark};

const HANDLE_COUNTS: [usize; 4] = [1, 2, 4, 8];
const CONCURRENT_THREAD_COUNTS: [usize; 6] = [1, 2, 4, 8, 16, 32];
const ITERATIONS_PER_THREAD: usize = 1000;

fn handle_call_resolution_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_call_resolution");
    group.measurement_time(Duration::from_secs(10));

    for &count in &HANDLE_COUNTS {
        let mut benchmark = MultiHandleCallBenchmark::new(count);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("handles", count), &count, |b, _| {
            b.iter(|| benchmark.run());
        });
    }

    group.finish();

    let mut group_concurrent = c.benchmark_group("handle_runtime_resolution/concurrent");
    group_concurrent.measurement_time(Duration::from_secs(10));

    for &threads in &CONCURRENT_THREAD_COUNTS {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group_concurrent.throughput(Throughput::Elements(attempts as u64));
        group_concurrent.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                let pool = ConcurrentHandleResolutionBenchmark::new(threads, ITERATIONS_PER_THREAD);
                b.iter(|| pool.run_batch());
            },
        );
    }

    group_concurrent.finish();
}

criterion_group!(benches, handle_call_resolution_benchmarks);
criterion_main!(benches);
