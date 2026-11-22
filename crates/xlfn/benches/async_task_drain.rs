use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::{AsyncTaskDrainBenchmark, benchmark_measurement_time};

fn drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_task_drain");
    group.measurement_time(benchmark_measurement_time());
    for count in [0, 1, 4, 5, 32, 128] {
        group.bench_function(BenchmarkId::from_parameter(count), |b| {
            b.iter_batched_ref(
                || AsyncTaskDrainBenchmark::new(count),
                |benchmark| assert_eq!(benchmark.run(), count),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();

    // The drain-only benchmark intentionally excludes the cost of creating
    // the calculation and publishing its first task.
    let mut lifecycle = c.benchmark_group("async_task_calculation_lifecycle");
    lifecycle.measurement_time(benchmark_measurement_time());
    for count in [0, 1, 32, 128] {
        lifecycle.bench_function(BenchmarkId::from_parameter(count), |b| {
            b.iter(|| assert_eq!(AsyncTaskDrainBenchmark::new(count).run(), count));
        });
    }
    lifecycle.finish();
}

criterion_group!(benches, drain);
criterion_main!(benches);
