use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{NumericArrayOutputBenchmark, benchmark_measurement_time};

fn array_numeric_output_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_numeric_output");
    group.measurement_time(benchmark_measurement_time());

    for cells in [1_000, 100_000] {
        let benchmark = NumericArrayOutputBenchmark::new(cells);
        group.throughput(Throughput::Elements(cells as u64));
        // Both construction paths calculate the same finite result cells and
        // include production return admission, publication, and cleanup.
        group.bench_function(BenchmarkId::new("matrix_build_return", cells), |b| {
            b.iter(|| benchmark.run_matrix());
        });
        group.bench_function(BenchmarkId::new("builder_build_return", cells), |b| {
            b.iter(|| benchmark.run_direct());
        });
        // Keep the cost of returning an existing owned Matrix visible too.
        // Fixture construction is outside the timer for this case only.
        group.bench_function(BenchmarkId::new("matrix_convert_return", cells), |b| {
            b.iter_batched(
                || benchmark.prepared_matrix(),
                |matrix| benchmark.run_prepared_matrix(matrix),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, array_numeric_output_benchmarks);
criterion_main!(benches);
