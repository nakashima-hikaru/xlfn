use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{ConcurrentAsyncAdmissionBenchmark, benchmark_measurement_time};

fn concurrent_admission(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_admission_concurrent");
    group.measurement_time(benchmark_measurement_time());
    for caller_count in [4, 32] {
        let benchmark = ConcurrentAsyncAdmissionBenchmark::new(caller_count);
        let calls_per_caller = 64;
        group.throughput(Throughput::Elements(
            (caller_count * calls_per_caller) as u64,
        ));
        group.bench_function(BenchmarkId::new("scalar", caller_count), |b| {
            b.iter(|| std::hint::black_box(benchmark.run_and_drain(calls_per_caller)));
        });
    }
    group.finish();
}
criterion_group!(benches, concurrent_admission);
criterion_main!(benches);
