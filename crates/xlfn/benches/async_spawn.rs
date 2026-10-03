use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    AsyncSpawnBenchmark, AsyncSpawnKind, CancellationLifecycleBenchmark, benchmark_measurement_time,
};

const WORKER_COUNTS: [usize; 4] = [1, 4, 8, 16];
const PRODUCER_COUNTS: [usize; 4] = [1, 4, 16, 32];
const ITERATIONS_PER_THREAD: usize = 128;
const MATRIX_ITERATIONS_PER_THREAD: usize = 64;
const RESCHEDULE_YIELDS: usize = 4;

fn concurrent_spawns(c: &mut Criterion) {
    let mut lifecycle = c.benchmark_group("cancellation_lifecycle");
    lifecycle.measurement_time(benchmark_measurement_time());
    for workers in [1_usize, 4, 8, 32] {
        const CYCLES: usize = 10_000;
        for (label, dispatch_only) in [("allocate_release", false), ("dispatch_control", true)] {
            let benchmark = CancellationLifecycleBenchmark::new(workers, dispatch_only);
            lifecycle.throughput(Throughput::Elements((workers * CYCLES) as u64));
            lifecycle.bench_function(BenchmarkId::new(label, workers), |b| {
                b.iter(|| {
                    assert_eq!(benchmark.run(CYCLES), workers * CYCLES);
                });
            });
        }
    }
    lifecycle.finish();

    let mut returns = c.benchmark_group("async_return");
    returns.measurement_time(benchmark_measurement_time());
    returns.bench_function("scalar", |b| {
        b.iter(AsyncSpawnBenchmark::encode_scalar_return)
    });
    returns.finish();

    let mut group = c.benchmark_group("async_spawn/per_iteration");
    group.measurement_time(benchmark_measurement_time());

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
    group_scaling.measurement_time(benchmark_measurement_time());

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
    group_reschedule.measurement_time(benchmark_measurement_time());

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
    group_drain.measurement_time(benchmark_measurement_time());

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
