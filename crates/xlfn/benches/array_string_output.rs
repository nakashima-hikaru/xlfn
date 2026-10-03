use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    BorrowedStringArrayOutputBenchmark, ScalarOutputBenchmark, benchmark_measurement_time,
};

const CELLS: usize = 16_384;
const PAYLOAD_LEN: usize = 32;

fn array_string_output_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_string_output");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(CELLS as u64));

    let benchmark = BorrowedStringArrayOutputBenchmark::new(CELLS, PAYLOAD_LEN);
    group.bench_function(BenchmarkId::new("borrowed_str", CELLS), |b| {
        b.iter(|| benchmark.run_borrowed());
    });

    for (label, payload) in [
        ("ascii_1k", "x".repeat(1_024)),
        ("unicode_short", "日本語💡".to_owned()),
        ("unicode_1k", "日本語💡".repeat(80)),
        ("mixed", "ABC日本語123💡".repeat(3)),
        ("empty", String::new()),
    ] {
        let benchmark = BorrowedStringArrayOutputBenchmark::with_payload(CELLS, payload);
        group.bench_function(BenchmarkId::new(label, CELLS), |b| {
            b.iter(|| benchmark.run_borrowed());
        });
    }

    group.finish();
}

fn counted_utf16_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("callback_counted_utf16");
    group.measurement_time(benchmark_measurement_time());
    for (label, payload) in [
        ("ascii_short", "x".repeat(32)),
        ("ascii_1k", "x".repeat(1_024)),
        ("unicode_short", "日本語💡".to_owned()),
        ("unicode_1k", "日本語💡".repeat(80)),
        ("unicode_limit", "あ".repeat(32_767)),
    ] {
        group.throughput(Throughput::Bytes(payload.len() as u64));
        let benchmark = BorrowedStringArrayOutputBenchmark::with_payload(1, payload);
        group.bench_function(label, |b| {
            b.iter(|| benchmark.run_counted_utf16());
        });
    }
    group.finish();
}

fn scalar_output_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("scalar_output");
    group.measurement_time(benchmark_measurement_time());
    for (label, payload) in [
        ("ascii_short", "Ready".to_owned()),
        ("ascii_1k", "x".repeat(1_024)),
        ("ascii_limit", "x".repeat(32_767)),
        ("unicode_short", "日本語💡".to_owned()),
        ("unicode_1k", "日本語💡".repeat(80)),
        ("unicode_limit", "日".repeat(32_767)),
        ("empty", String::new()),
    ] {
        let benchmark = ScalarOutputBenchmark::new(payload);
        group.bench_function(format!("{label}/borrowed"), |b| {
            b.iter(|| benchmark.run_borrowed())
        });
        group.bench_function(format!("{label}/owned"), |b| {
            b.iter(|| benchmark.run_owned())
        });
    }
    let benchmark = ScalarOutputBenchmark::new("Ready".to_owned());
    group.bench_function("enum", |b| b.iter(|| benchmark.run_enum()));
    group.bench_function("number", |b| b.iter(|| benchmark.run_number()));
    group.finish();
}

criterion_group!(
    benches,
    array_string_output_benchmarks,
    counted_utf16_benchmarks,
    scalar_output_benchmarks
);
criterion_main!(benches);
