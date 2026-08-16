use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::InputIdentityDiagnosticBenchmark;

fn input_identity_diagnostic_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_identity_diag");
    group.measurement_time(Duration::from_secs(3));
    let benchmark = InputIdentityDiagnosticBenchmark::new();

    group.bench_function("f64/full_current", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_full_current()));
    });
    group.bench_function("f64/argument_only", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_argument_only()));
    });
    group.bench_function("f64/root_only", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_root_only()));
    });
    group.bench_function("f64/blake3_streaming_8b", |b| {
        b.iter(|| std::hint::black_box(benchmark.blake3_streaming_8b()));
    });
    group.bench_function("f64/blake3_one_shot_8b", |b| {
        b.iter(|| std::hint::black_box(benchmark.blake3_one_shot_8b()));
    });
    group.bench_function("owned_number/full_current", |b| {
        b.iter(|| std::hint::black_box(benchmark.owned_number_full_current()));
    });
    group.bench_function("string_short/full_current", |b| {
        b.iter(|| std::hint::black_box(benchmark.string_short_full_current()));
    });
    group.bench_function("f64/batched_64", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_batched_64()));
    });
    group.bench_function("f64/batched_256", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_batched_256()));
    });
    group.bench_function("f64/batched_1024", |b| {
        b.iter(|| std::hint::black_box(benchmark.f64_batched_1024()));
    });
    group.bench_function("matrix_f64_100k/batched_64", |b| {
        b.iter(|| std::hint::black_box(benchmark.matrix_f64_100k_batched_64()));
    });
    group.bench_function("matrix_f64_100k/batched_256", |b| {
        b.iter(|| std::hint::black_box(benchmark.matrix_f64_100k_batched_256()));
    });
    group.bench_function("matrix_f64_100k/batched_1024", |b| {
        b.iter(|| std::hint::black_box(benchmark.matrix_f64_100k_batched_1024()));
    });

    group.finish();
}

criterion_group!(benches, input_identity_diagnostic_benchmarks);
criterion_main!(benches);
