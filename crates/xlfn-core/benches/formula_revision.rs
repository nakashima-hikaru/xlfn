use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{FormulaRevisionBenchCase, FormulaRevisionBenchmark};

fn formula_revision_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("formula_revision/warm_hit");

    for case in FormulaRevisionBenchCase::END_TO_END {
        let benchmark = FormulaRevisionBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
        benchmark.assert_warm_hit();
    }

    group.finish();
}

criterion_group!(benches, formula_revision_benchmarks);
criterion_main!(benches);
