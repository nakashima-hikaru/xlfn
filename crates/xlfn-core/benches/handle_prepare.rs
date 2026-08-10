use criterion::{Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandlePrepareBenchmark, HandlePrepareKind};

fn handle_prepare_cold_miss(c: &mut Criterion) {
    let bench = HandlePrepareBenchmark::new();
    c.bench_function("handle_prepare_cold_miss", |b| {
        b.iter(|| bench.run(HandlePrepareKind::ColdMiss, 100));
    });
    drop(bench);
}

fn handle_prepare_warm_hit(c: &mut Criterion) {
    let bench = HandlePrepareBenchmark::new();
    c.bench_function("handle_prepare_warm_hit", |b| {
        b.iter(|| bench.run(HandlePrepareKind::WarmHit, 100));
    });
    drop(bench);
}

fn handle_prepare_same_key_contended(c: &mut Criterion) {
    let bench = HandlePrepareBenchmark::new();
    c.bench_function("handle_prepare_same_key_contended", |b| {
        b.iter(|| bench.run(HandlePrepareKind::Contended, 100));
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
