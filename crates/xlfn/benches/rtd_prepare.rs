use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{RtdPrepareBenchmark, benchmark_measurement_time};

fn rtd_prepare_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_prepare");
    group.measurement_time(benchmark_measurement_time());
    const COUNT: usize = 256;
    group.throughput(Throughput::Elements(COUNT as u64));
    for parts in [1, 10] {
        for operation in ["new", "existing", "churn"] {
            group.bench_function(BenchmarkId::new(operation, parts), |b| {
                b.iter_batched_ref(
                    || RtdPrepareBenchmark::new(COUNT, parts, operation == "existing"),
                    |bench| {
                        if operation == "churn" {
                            bench.run_churn();
                        } else {
                            bench.run_prepare();
                        }
                    },
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();

    let mut group = c.benchmark_group("rtd_subscribe_input");
    group.measurement_time(benchmark_measurement_time());
    for (count, parts) in [(256, 1), (256, 10), (4096, 1)] {
        group.throughput(Throughput::Elements(count as u64));
        for operation in ["new", "existing", "churn"] {
            group.bench_function(
                BenchmarkId::new(operation, format!("{count}-topics-{parts}-parts")),
                |b| {
                    b.iter_batched_ref(
                        || RtdPrepareBenchmark::new(count, parts, operation == "existing"),
                        |bench| bench.run_subscribe_input(operation == "churn"),
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, rtd_prepare_benchmarks);
criterion_main!(benches);
