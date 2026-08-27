use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{AsyncSpawnBenchmark, AsyncSpawnKind, BENCHMARK_MEASUREMENT_TIME};

const WORKER_COUNTS: [usize; 4] = [1, 4, 8, 16];
const PRODUCER_COUNTS: [usize; 4] = [1, 4, 16, 32];
const ITERATIONS_PER_THREAD: usize = 128;
const MATRIX_ITERATIONS_PER_THREAD: usize = 64;
const RESCHEDULE_YIELDS: usize = 4;

fn concurrent_spawns(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_spawn/per_iteration");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for threads in [1_usize, 4, 16, 32] {
        let attempts = threads * ITERATIONS_PER_THREAD;
        group.throughput(Throughput::Elements(attempts as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched_ref(
                    || AsyncSpawnBenchmark::new(4, threads),
                    |benchmark| {
                        let result = benchmark.run(ITERATIONS_PER_THREAD);
                        assert_eq!(result.other_errors, 0);
                        assert_eq!(result.overloaded, 0);
                        assert_eq!(result.accepted, attempts);
                        std::hint::black_box(result);
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();

    let mut group_scaling = c.benchmark_group("async_spawn/matrix_spawn");
    group_scaling.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for &workers in &WORKER_COUNTS {
        for &producers in &PRODUCER_COUNTS {
            let attempts = producers * MATRIX_ITERATIONS_PER_THREAD;
            group_scaling.throughput(Throughput::Elements(attempts as u64));
            group_scaling.bench_with_input(
                BenchmarkId::new(format!("workers_{workers}"), producers),
                &(workers, producers),
                |b, &(workers, producers)| {
                    b.iter_batched_ref(
                        || AsyncSpawnBenchmark::new(workers, producers),
                        |benchmark| {
                            let result = benchmark.run(MATRIX_ITERATIONS_PER_THREAD);
                            assert_eq!(result.other_errors, 0);
                            assert_eq!(result.overloaded, 0);
                            assert_eq!(result.accepted, attempts);
                            std::hint::black_box(result);
                        },
                        BatchSize::PerIteration,
                    );
                },
            );
        }
    }
    group_scaling.finish();

    let mut group_reschedule = c.benchmark_group("async_spawn/matrix_reschedule");
    group_reschedule.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for &workers in &WORKER_COUNTS {
        for &producers in &PRODUCER_COUNTS {
            let attempts = producers * MATRIX_ITERATIONS_PER_THREAD;
            group_reschedule.throughput(Throughput::Elements(attempts as u64));
            group_reschedule.bench_with_input(
                BenchmarkId::new(format!("workers_{workers}"), producers),
                &(workers, producers),
                |b, &(workers, producers)| {
                    b.iter_batched_ref(
                        || {
                            AsyncSpawnBenchmark::new_with_kind(
                                workers,
                                producers,
                                AsyncSpawnKind::Reschedule(RESCHEDULE_YIELDS),
                            )
                        },
                        |benchmark| {
                            let result = benchmark.run(MATRIX_ITERATIONS_PER_THREAD);
                            assert_eq!(result.other_errors, 0);
                            assert_eq!(result.overloaded, 0);
                            assert_eq!(result.accepted, attempts);
                            std::hint::black_box(result);
                        },
                        BatchSize::PerIteration,
                    );
                },
            );
        }
    }
    group_reschedule.finish();

    let mut group_drain = c.benchmark_group("async_spawn/spawn_and_drain");
    group_drain.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for &workers in &WORKER_COUNTS {
        for &producers in &PRODUCER_COUNTS {
            let attempts = producers * MATRIX_ITERATIONS_PER_THREAD;
            group_drain.throughput(Throughput::Elements(attempts as u64));
            group_drain.bench_with_input(
                BenchmarkId::new(format!("workers_{workers}"), producers),
                &(workers, producers),
                |b, &(workers, producers)| {
                    let benchmark = AsyncSpawnBenchmark::new_with_kind(
                        workers,
                        producers,
                        AsyncSpawnKind::Reschedule(RESCHEDULE_YIELDS),
                    );
                    b.iter(|| {
                        let result = benchmark.run_and_drain(MATRIX_ITERATIONS_PER_THREAD);
                        assert_eq!(result.other_errors, 0);
                        assert_eq!(result.overloaded, 0);
                        assert_eq!(result.accepted, attempts);
                        std::hint::black_box(result);
                    });
                },
            );
        }
    }
    group_drain.finish();
}

criterion_group!(benches, concurrent_spawns);
criterion_main!(benches);
