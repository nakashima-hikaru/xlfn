use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{
    HandleColdBatch, HandleContendedBenchmark, HandleWarmBenchmark,
};

fn handle_prepare_cold_miss(c: &mut Criterion) {
    c.bench_function("handle_prepare_cold_miss", |b| {
        b.iter_batched_ref(
            || HandleColdBatch::new(100),
            |batch| batch.run(),
            BatchSize::SmallInput,
        );
    });
}

fn handle_prepare_warm_hit(c: &mut Criterion) {
    let bench = HandleWarmBenchmark::new();
    c.bench_function("handle_prepare_warm_hit", |b| {
        b.iter(|| bench.run(100));
    });
    drop(bench);
}

fn handle_prepare_same_key_contended(c: &mut Criterion) {
    let bench = HandleContendedBenchmark::new();
    c.bench_function("handle_prepare_same_key_contended", |b| {
        b.iter(|| bench.run());
    });
    drop(bench);
}

criterion_group!(
    benches,
    handle_prepare_cold_miss,
    handle_prepare_warm_hit,
    handle_prepare_same_key_contended,
);
criterion_main!(benches);
