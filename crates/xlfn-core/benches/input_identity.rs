use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{
    FormulaRevisionBenchCase, InputIdentityBenchmark, TypedInputIdentityBenchCase,
    TypedInputIdentityBenchmark,
};

fn input_identity_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_identity");
    group.measurement_time(Duration::from_secs(10));

    for case in FormulaRevisionBenchCase::ALL {
        let benchmark = InputIdentityBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();

    let typed_benchmark = TypedInputIdentityBenchmark::new();
    let mut typed_group = c.benchmark_group("input_identity_typed");
    typed_group.measurement_time(Duration::from_secs(10));
    for case in TypedInputIdentityBenchCase::ALL {
        typed_group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(typed_benchmark.run(case)));
        });
    }
    typed_group.finish();
}

criterion_group!(benches, input_identity_benchmarks);
criterion_main!(benches);
