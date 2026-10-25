use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{RegistrationBenchmark, benchmark_measurement_time};

fn registration_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("registration");
    group.measurement_time(benchmark_measurement_time());
    for count in [10, 100, 1_000, 5_000] {
        let benchmark = RegistrationBenchmark::new(count);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(BenchmarkId::new("preflight", count), |b| {
            b.iter(|| benchmark.run_preflight());
        });
        group.bench_function(BenchmarkId::new("host", count), |b| {
            b.iter(|| benchmark.run_registration());
        });
        group.bench_function(BenchmarkId::new("total", count), |b| {
            b.iter(|| benchmark.run_total());
        });
    }
    group.finish();
}

criterion_group!(benches, registration_benchmarks);
criterion_main!(benches);
