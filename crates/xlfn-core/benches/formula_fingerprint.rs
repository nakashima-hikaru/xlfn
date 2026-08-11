use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{FingerprintBenchCase, FingerprintBenchmark};

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

criterion_group!(benches, formula_fingerprint_benchmarks);
criterion_main!(benches);
