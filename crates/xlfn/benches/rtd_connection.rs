use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{benchmark_measurement_time, with_rtd_connection_benchmark};

fn rtd_connection_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_connection");
    group.measurement_time(benchmark_measurement_time());
    for count in [1, 32, 256] {
        group.throughput(Throughput::Elements(count as u64));
        for adapter_only in [true, false] {
            let case = if adapter_only { "adapter" } else { "runtime" };
            group.bench_function(BenchmarkId::new(case, count), |b| {
                with_rtd_connection_benchmark(count, adapter_only, true, |operation| {
                    b.iter(operation);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, rtd_connection_benchmarks);
criterion_main!(benches);
