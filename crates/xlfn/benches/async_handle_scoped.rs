use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{HandleScopedDeliveryBenchmark, benchmark_measurement_time};

fn scoped(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_handle_scoped");
    group.measurement_time(benchmark_measurement_time());
    for instrumented in [false, true] {
        let benchmark = HandleScopedDeliveryBenchmark::new(instrumented);
        benchmark.run(256);
        for count in [1, 128] {
            group.throughput(Throughput::Elements(count as u64));
            group.bench_with_input(
                BenchmarkId::new(
                    if instrumented {
                        "instrumented"
                    } else {
                        "uninstrumented"
                    },
                    count,
                ),
                &count,
                |b, &count| b.iter(|| benchmark.run(count)),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, scoped);
criterion_main!(benches);
