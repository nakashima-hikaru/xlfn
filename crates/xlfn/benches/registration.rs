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

    const METADATA_COUNT: usize = 1_000;
    let mut group = c.benchmark_group("registration_metadata");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(METADATA_COUNT as u64));
    for arity in [1, 4, 6, 16] {
        for text_units in [32, 64, 128] {
            let benchmark = RegistrationBenchmark::with_metadata(METADATA_COUNT, arity, text_units);
            group.bench_function(
                BenchmarkId::new(format!("host/{arity}_args"), text_units),
                |b| {
                    b.iter(|| benchmark.run_registration());
                },
            );
            if arity == 6 && text_units == 64 {
                group.bench_function("preflight_long_names", |b| {
                    b.iter(|| benchmark.run_preflight())
                });
            }
        }
    }
    group.finish();
}

criterion_group!(benches, registration_benchmarks);
criterion_main!(benches);
