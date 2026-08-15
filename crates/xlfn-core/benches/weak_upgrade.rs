use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{WeakUpgradeBenchCase, WeakUpgradeBenchmark};

const ITERATIONS_PER_WORKER: usize = 1_000;
const THREAD_COUNTS: [usize; 4] = [1, 4, 16, 32];

fn weak_upgrade_benchmarks(c: &mut Criterion) {
    for case in [
        WeakUpgradeBenchCase::SameObject,
        WeakUpgradeBenchCase::DistinctObjects,
    ] {
        let mut group = c.benchmark_group(case.name());
        group.measurement_time(Duration::from_secs(10));

        for workers in THREAD_COUNTS {
            let benchmark = WeakUpgradeBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
            group.bench_with_input(BenchmarkId::from_parameter(workers), &workers, |b, _| {
                b.iter(|| benchmark.run())
            });
        }

        group.finish();
    }
}

criterion_group!(benches, weak_upgrade_benchmarks);
criterion_main!(benches);
