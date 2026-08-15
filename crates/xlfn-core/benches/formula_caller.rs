use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{FormulaCallerBenchCase, FormulaCallerBenchmark};

fn formula_caller_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve_formula_caller");
    group.measurement_time(Duration::from_secs(10));

    for case in [FormulaCallerBenchCase::Ref, FormulaCallerBenchCase::SRef] {
        let benchmark = FormulaCallerBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();
}

criterion_group!(benches, formula_caller_benchmarks);
criterion_main!(benches);
