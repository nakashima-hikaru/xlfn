use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{NumericArrayOutputBenchmark, benchmark_measurement_time};
use xlfn::output::NumericArrayBuilder;

fn array_numeric_output_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_numeric_output");
    group.measurement_time(benchmark_measurement_time());

    for cells in [100, 1_000, 10_000, 100_000] {
        let benchmark = NumericArrayOutputBenchmark::new(cells);
        group.throughput(Throughput::Elements(cells as u64));
        // Both construction paths calculate the same finite result cells and
        // include production return admission and publication. FP12 storage is reclaimed
        // at the next outer invocation; the final result remains retained.
        group.bench_function(BenchmarkId::new("matrix_build_return", cells), |b| {
            b.iter(|| benchmark.run_matrix());
        });
        group.bench_function(BenchmarkId::new("builder_build_return", cells), |b| {
            b.iter(|| benchmark.run_direct());
        });
        group.bench_function(BenchmarkId::new("matrix_build_return_q", cells), |b| {
            b.iter(|| benchmark.run_matrix_q());
        });
        group.bench_function(BenchmarkId::new("builder_build_return_q", cells), |b| {
            b.iter(|| benchmark.run_direct_q());
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
        let values = benchmark.prepared_matrix().into_vec();
        group.bench_function(
            BenchmarkId::new("numeric_slice_convert_return", cells),
            |b| {
                b.iter(|| benchmark.run_numeric_slice(&values));
            },
        );
        group.bench_function(
            BenchmarkId::new("numeric_slice_convert_return_q", cells),
            |b| {
                b.iter(|| benchmark.run_numeric_slice_q(&values));
            },
        );
    }
    group.finish();

    // Keep the producer in the downstream crate, like BENCH.MAT.MAKE. An
    // internal fixture can hide the cost of crossing the public builder API.
    let mut group = c.benchmark_group("array_numeric_producer");
    group.measurement_time(benchmark_measurement_time());
    for cells in [100, 10_000, 100_000] {
        group.bench_function(BenchmarkId::new("push_f64", cells), |b| {
            b.iter(|| {
                let cells = std::hint::black_box(cells);
                let mut output = NumericArrayBuilder::new(cells, 1).unwrap();
                let seed = std::hint::black_box(0.0);
                for index in 0..cells {
                    output.push_f64(seed + index as f64).unwrap();
                }
                std::hint::black_box(output.finish().unwrap());
            });
        });
    }
    group.finish();
}

criterion_group!(benches, array_numeric_output_benchmarks);
criterion_main!(benches);
