use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    BENCHMARK_MEASUREMENT_TIME, RtdPublishNumberBenchmark, RtdPublishStringBenchmark,
};

const ITERATIONS: usize = 10_000;

fn rtd_publish_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_publish");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);
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
        BenchmarkId::new("number", "repeated_same"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_repeated_same(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("number", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_changing(iterations));
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

    group.bench_with_input(
        BenchmarkId::new("string", "repeated_same"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_repeated_same(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "caller_publish"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_string_allocated_publish(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "conversion"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_string_conversion(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "stored_publish"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_stored_publish(iterations));
        },
    );

    group.finish();
}

criterion_group!(benches, rtd_publish_benchmarks);
criterion_main!(benches);
