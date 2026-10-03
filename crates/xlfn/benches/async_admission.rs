use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{AsyncAdmissionBenchmark, benchmark_measurement_time};

fn admission(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_admission");
    group.measurement_time(benchmark_measurement_time());
    for saturated in [false, true] {
        for (label, elements) in [("scalar", None), ("matrix_100k", Some(100_000))] {
            let mut benchmark = AsyncAdmissionBenchmark::new(elements, saturated);
            let calls = if saturated { 1 } else { 64 };
            group.throughput(Throughput::Elements(calls as u64));
            group.bench_function(
                BenchmarkId::new(if saturated { "saturated" } else { "admitted" }, label),
                |b| {
                    b.iter(|| std::hint::black_box(benchmark.run(calls)));
                },
            );
        }
    }
    group.finish();
}
criterion_group!(benches, admission);
criterion_main!(benches);
