use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{BENCHMARK_MEASUREMENT_TIME, BorrowedStringArrayOutputBenchmark};

const CELLS: usize = 16_384;
const PAYLOAD_LEN: usize = 32;

fn array_string_output_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_string_output");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);
    group.throughput(Throughput::Elements(CELLS as u64));

    let benchmark = BorrowedStringArrayOutputBenchmark::new(CELLS, PAYLOAD_LEN);
    group.bench_function(BenchmarkId::new("borrowed_str", CELLS), |b| {
        b.iter(|| benchmark.run_borrowed());
    });

    group.finish();
}

criterion_group!(benches, array_string_output_benchmarks);
criterion_main!(benches);
