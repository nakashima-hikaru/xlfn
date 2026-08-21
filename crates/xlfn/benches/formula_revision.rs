use criterion::{Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::FormulaRevisionBenchmark;
use xlfn::value::Matrix;

fn formula_revision_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("formula_revision/warm_hit");

    let f64_value = FormulaRevisionBenchmark::new(42.0_f64);
    group.bench_function("f64", |b| {
        b.iter(|| std::hint::black_box(f64_value.run()));
    });
    f64_value.assert_warm_hit();

    let string_value = FormulaRevisionBenchmark::new(String::from("short"));
    group.bench_function("string_short", |b| {
        b.iter(|| std::hint::black_box(string_value.run()));
    });
    string_value.assert_warm_hit();

    let matrix = Matrix::new(10, 10_000, (0..100_000).map(|index| index as f64).collect())
        .expect("benchmark matrix dimensions must be valid");
    let matrix_value = FormulaRevisionBenchmark::new(matrix);
    group.bench_function("matrix_f64_100k", |b| {
        b.iter(|| std::hint::black_box(matrix_value.run()));
    });
    matrix_value.assert_warm_hit();

    group.finish();
}

criterion_group!(benches, formula_revision_benchmarks);
criterion_main!(benches);
