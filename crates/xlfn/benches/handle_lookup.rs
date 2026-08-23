use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    BENCHMARK_MEASUREMENT_TIME, HandleLookupBenchCase, HandleLookupBenchmark,
};

const ITERATIONS_PER_WORKER: usize = 1_000;
const THREAD_COUNTS: [usize; 4] = [1, 4, 16, 32];

fn handle_lookup_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_lookup");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for case in [
        HandleLookupBenchCase::WarmSameToken,
        HandleLookupBenchCase::DistinctTokens,
    ] {
        for workers in THREAD_COUNTS {
            let benchmark = HandleLookupBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
            group.bench_with_input(BenchmarkId::new(case.name(), workers), &workers, |b, _| {
                b.iter(|| benchmark.run())
            });
        }
    }

    group.finish();
}

criterion_group!(benches, handle_lookup_benchmarks);
criterion_main!(benches);
