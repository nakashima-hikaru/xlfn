use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{ArcHandleLookupBenchmark, BENCHMARK_MEASUREMENT_TIME};

const ITERATIONS_PER_WORKER: usize = 1_000;
const THREAD_COUNTS: [usize; 4] = [1, 4, 16, 32];

fn arc_control_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_lookup/diagnostic_arc_control");
    group.measurement_time(BENCHMARK_MEASUREMENT_TIME);

    for workers in THREAD_COUNTS {
        let benchmark = ArcHandleLookupBenchmark::new(workers, ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_with_input(
            BenchmarkId::new("arc_control", workers),
            &workers,
            |b, _| b.iter(|| benchmark.run()),
        );
    }

    group.finish();
}

criterion_group!(benches, arc_control_benchmarks);
criterion_main!(benches);
