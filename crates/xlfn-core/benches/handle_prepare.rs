use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{
    FormatFormulaTopicKeyBenchmark, HandleColdBatch, HandleContendedBenchmark,
    HandleDistinctKeyBenchmark, HandleGuardContentionBenchmark, HandleKeyAllocationBenchmark,
    HandleWarmBenchmark,
};

const DISTINCT_WORKERS: [usize; 6] = [1, 2, 4, 8, 16, 32];
const DISTINCT_ITERATIONS_PER_WORKER: usize = 1_000;

fn handle_prepare_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_prepare");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("cold_miss", |b| {
        b.iter_batched_ref(
            || HandleColdBatch::new(100),
            |batch| batch.run(),
            BatchSize::SmallInput,
        );
    });
    let bench = HandleWarmBenchmark::new();
    group.bench_function("warm_hit", |b| {
        b.iter(|| bench.run(100));
    });

    drop(bench);

    let bench = HandleContendedBenchmark::new();
    group.bench_function("same_key_contended", |b| {
        b.iter(|| bench.run());
    });
    drop(bench);

    for workers in DISTINCT_WORKERS {
        let bench = HandleDistinctKeyBenchmark::new(workers, DISTINCT_ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(bench.total_iterations() as u64));
        group.bench_with_input(
            BenchmarkId::new("distinct_key", workers),
            &bench,
            |b, bench| {
                b.iter(|| {
                    bench.run();
                    std::hint::black_box(())
                })
            },
        );
        bench.assert_warm_hit();

        let bench = HandleKeyAllocationBenchmark::new(workers, DISTINCT_ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(bench.total_iterations() as u64));
        group.bench_with_input(
            BenchmarkId::new("allocation_only", workers),
            &bench,
            |b, bench| {
                b.iter(|| {
                    bench.run();
                    std::hint::black_box(())
                })
            },
        );

        let bench = HandleGuardContentionBenchmark::new(workers, DISTINCT_ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(bench.total_iterations() as u64));
        group.bench_with_input(
            BenchmarkId::new("guards_only", workers),
            &bench,
            |b, bench| {
                b.iter(|| {
                    bench.run();
                    std::hint::black_box(())
                })
            },
        );
    }

    group.finish();
}

fn handle_key_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_key");
    let benchmark = FormatFormulaTopicKeyBenchmark::new();

    group.bench_function("format_formula_topic_key", |b| {
        b.iter(|| std::hint::black_box(benchmark.run()));
    });

    group.finish();
}

criterion_group!(benches, handle_prepare_benchmarks, handle_key_benchmarks);
criterion_main!(benches);
