use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{RegistryCacheBenchmark, benchmark_measurement_time};

const ITERATIONS_PER_WORKER: usize = 10_000;

fn cache_registry_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_registry");
    group.measurement_time(benchmark_measurement_time());

    for workers in [1, 8, 32] {
        let benchmark = RegistryCacheBenchmark::distinct_endpoints(workers, ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("distinct_endpoints", workers), |b| {
            b.iter(|| benchmark.run());
        });
    }

    for endpoints in [1, 8, 16] {
        let benchmark = RegistryCacheBenchmark::endpoint_cycle(endpoints, ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("endpoint_cycle", endpoints), |b| {
            b.iter(|| benchmark.run());
        });
    }

    group.finish();
}

criterion_group!(benches, cache_registry_benchmarks);
criterion_main!(benches);
