use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RtdExistingBreakdownBenchmark, RtdExistingBreakdownCase, benchmark_measurement_time,
};

fn existing_breakdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_existing_breakdown");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(256));
    for case in RtdExistingBreakdownCase::ALL {
        for parts in [1, 10] {
            group.bench_function(BenchmarkId::new(case.name(), parts), |b| {
                b.iter_batched_ref(
                    || RtdExistingBreakdownBenchmark::new(256, parts, case),
                    |fixture| fixture.run(case),
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, existing_breakdown);
criterion_main!(benches);
