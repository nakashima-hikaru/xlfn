use criterion::{Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::{BENCHMARK_MEASUREMENT_TIME, SemanticIdentityBenchmark};
use xlfn::value::Matrix;

fn input_identity_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_identity");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    let f64_value = SemanticIdentityBenchmark::new(42.0_f64);
    group.bench_function("f64", |b| {
        b.iter(|| std::hint::black_box(f64_value.run()));
    });

    let string_value = SemanticIdentityBenchmark::new(String::from("short"));
    group.bench_function("string_short", |b| {
        b.iter(|| std::hint::black_box(string_value.run()));
    });

    let matrix = Matrix::new(10, 10_000, (0..100_000).map(|index| index as f64).collect())
        .expect("benchmark matrix dimensions must be valid");
    let matrix_value = SemanticIdentityBenchmark::new(matrix);
    group.bench_function("matrix_f64_100k", |b| {
        b.iter(|| std::hint::black_box(matrix_value.run()));
    });

    group.finish();
}

criterion_group!(benches, input_identity_benchmarks);
criterion_main!(benches);
