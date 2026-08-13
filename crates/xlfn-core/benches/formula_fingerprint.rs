use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandleFormulaBenchCase, XloperFingerprintBenchmark};

fn xloper_fingerprint_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("xloper_fingerprint");
    group.measurement_time(Duration::from_secs(10));

    for case in HandleFormulaBenchCase::ALL {
        let benchmark = XloperFingerprintBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();
}

criterion_group!(benches, xloper_fingerprint_benchmarks);
criterion_main!(benches);
