use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandleFormulaBenchCase, XloperFingerprintBenchmark};

fn fingerprint_current_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("fingerprint_current");
    group.measurement_time(Duration::from_secs(10));

    for case in HandleFormulaBenchCase::ALL {
        let benchmark = XloperFingerprintBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();
}

fn fingerprint_diagnostic_benchmarks(c: &mut Criterion) {
    let mut scan_group = c.benchmark_group("fingerprint_scan_only");
    scan_group.measurement_time(Duration::from_secs(10));
    for case in HandleFormulaBenchCase::END_TO_END {
        let benchmark = XloperFingerprintBenchmark::new(case);
        scan_group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.scan_only()));
        });
    }
    scan_group.finish();

    let mut encode_group = c.benchmark_group("fingerprint_encode_no_hash");
    encode_group.measurement_time(Duration::from_secs(10));
    for case in HandleFormulaBenchCase::END_TO_END {
        let benchmark = XloperFingerprintBenchmark::new(case);
        encode_group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.encode_no_hash()));
        });
    }
    encode_group.finish();

    let mut hash_group = c.benchmark_group("fingerprint_hash_preencoded");
    hash_group.measurement_time(Duration::from_secs(10));
    for case in HandleFormulaBenchCase::END_TO_END {
        let benchmark = XloperFingerprintBenchmark::new(case);
        hash_group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.hash_preencoded()));
        });
    }
    hash_group.finish();
}

criterion_group!(
    benches,
    fingerprint_current_benchmarks,
    fingerprint_diagnostic_benchmarks
);
criterion_main!(benches);
