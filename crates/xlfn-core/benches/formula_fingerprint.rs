use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{
    FingerprintBenchCase, FingerprintBenchmark, HandleFormulaBenchCase, XloperFingerprintBenchmark,
};

fn formula_fingerprint_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("formula_fingerprint");

    for case in FingerprintBenchCase::ALL {
        let benchmark = FingerprintBenchmark::new(case);
        group.throughput(Throughput::Bytes(benchmark.encoded_bytes() as u64));

        group.bench_with_input(
            BenchmarkId::new("selected", case.name()),
            &benchmark,
            |b, benchmark| {
                b.iter(|| std::hint::black_box(benchmark.run_selected()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("direct", case.name()),
            &benchmark,
            |b, benchmark| {
                b.iter(|| std::hint::black_box(benchmark.run_direct()));
            },
        );
    }

    group.finish();
}

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

criterion_group!(
    benches,
    formula_fingerprint_benchmarks,
    xloper_fingerprint_benchmarks
);
criterion_main!(benches);
