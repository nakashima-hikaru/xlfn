use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{RtdPublishNumberBenchmark, RtdPublishStringBenchmark};

const ITERATIONS: usize = 10_000;

fn rtd_publish_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_publish");
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(ITERATIONS as u64));

    group.bench_with_input(
        BenchmarkId::new("number", "coalesced"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_coalesced(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("number", "drain_each"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_drain_each(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "coalesced"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_coalesced(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "drain_each"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_drain_each(iterations));
        },
    );

    group.finish();
}

criterion_group!(benches, rtd_publish_benchmarks);
criterion_main!(benches);
