use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{InputIdentityBenchCase, InputIdentityBenchmark};

fn input_identity_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_identity");
    group.measurement_time(Duration::from_secs(10));

    for case in InputIdentityBenchCase::ALL {
        let benchmark = InputIdentityBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();
}

criterion_group!(benches, input_identity_benchmarks);
criterion_main!(benches);
