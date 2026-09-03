use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    BENCHMARK_MEASUREMENT_TIME, HandleColdBatch, HandleColdGrowthBenchmark,
    HandleDistinctKeyBenchmark, HandleRevisionChurnBenchmark, HandleWarmBenchmark,
};

const DISTINCT_WORKERS: [usize; 4] = [1, 4, 16, 32];
const DISTINCT_ITERATIONS_PER_WORKER: usize = 1_000;
const BATCH_SIZE: usize = 100;
const COLD_GROW_SIZES: [usize; 3] = [1_000, 10_000, 100_000];
const REVISION_CHURN_SIZE: usize = 10_000;

fn handle_prepare_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_prepare");
    group.sample_size(50);
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    group.throughput(Throughput::Elements(BATCH_SIZE as u64));
    group.bench_function("cold_miss_batch_100", |b| {
        b.iter_batched_ref(
            || HandleColdBatch::new(BATCH_SIZE),
            |batch| batch.run(),
            BatchSize::SmallInput,
        );
    });
    let bench = HandleWarmBenchmark::new();
    group.bench_function("warm_hit_batch_100", |b| {
        b.iter(|| bench.run(BATCH_SIZE));
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
    }

    // Cold growth at scale (measuring handle table insertion scaling)
    for size in COLD_GROW_SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("cold_grow", size), &size, |b, &size| {
            b.iter_batched_ref(
                || HandleColdGrowthBenchmark::new(size),
                |bench| bench.run(),
                BatchSize::LargeInput,
            );
        });
    }

    // Revision churn at scale
    group.throughput(Throughput::Elements(REVISION_CHURN_SIZE as u64));
    let churn_bench = HandleRevisionChurnBenchmark::new(REVISION_CHURN_SIZE, REVISION_CHURN_SIZE);
    group.bench_function("revision_churn/10_000", |b| {
        b.iter(|| churn_bench.run());
    });
    drop(churn_bench);

    group.finish();
}

criterion_group!(benches, handle_prepare_benchmarks);
criterion_main!(benches);
