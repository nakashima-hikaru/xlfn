use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RtdPublishNumberBenchmark, RtdPublishStringBenchmark, benchmark_measurement_time,
};

const ITERATIONS: usize = 10_000;

fn rtd_publish_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_publish");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(ITERATIONS as u64));

    group.bench_with_input(
        BenchmarkId::new("number", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("number", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
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
        BenchmarkId::new("string", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_repeated_same(iterations));
        },
    );

    let bench_8k = RtdPublishStringBenchmark::with_payload_len(8 * 1024);
    group.bench_with_input(
        BenchmarkId::new("string_8k", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            b.iter(|| bench_8k.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string_8k", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            b.iter(|| bench_8k.run_repeated_same(iterations));
        },
    );

    group.finish();
}

criterion_group!(benches, rtd_publish_benchmarks);
criterion_main!(benches);
