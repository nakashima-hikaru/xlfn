use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandleFormulaBenchCase, HandleFormulaBenchmark};

fn handle_formula_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_formula");

    for case in HandleFormulaBenchCase::ALL {
        let benchmark = HandleFormulaBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
        benchmark.assert_warm_hit();
    }

    group.finish();
}

criterion_group!(benches, handle_formula_benchmarks);
criterion_main!(benches);
