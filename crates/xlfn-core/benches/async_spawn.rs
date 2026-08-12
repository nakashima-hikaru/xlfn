use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::AsyncSpawnBenchmark;

const WORKERS: usize = 4;
const ITERATIONS_PER_THREAD: usize = 128;

fn concurrent_spawns(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_spawn/concurrent");
    group.measurement_time(Duration::from_secs(10));

    for threads in [1_usize, 2, 4, 8, 16, 32] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group.throughput(Throughput::Elements(attempts as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched_ref(
                    || AsyncSpawnBenchmark::new(WORKERS, threads),
                    |benchmark| {
                        let result = benchmark.run(ITERATIONS_PER_THREAD);
                        assert_eq!(result.other_errors, 0);
                        assert_eq!(result.overloaded, 0);
                        assert_eq!(result.accepted, attempts);
                        std::hint::black_box(result);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, concurrent_spawns);
criterion_main!(benches);
