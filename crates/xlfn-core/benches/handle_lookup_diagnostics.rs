use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandleLookupDiagnosticCase, HandleLookupDiagnostics};

const WORKERS: [usize; 6] = [1, 2, 4, 8, 16, 32];
const ITERATIONS_PER_WORKER: usize = 1_000;

fn handle_lookup_diagnostics(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_lookup_diagnostics");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));

    for case in HandleLookupDiagnosticCase::ALL {
        for workers in WORKERS {
            let benchmark = HandleLookupDiagnostics::new(case, workers, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
            group.bench_with_input(
                BenchmarkId::new(case.name(), workers),
                &benchmark,
                |b, benchmark| {
                    b.iter(|| {
                        benchmark.run();
                        std::hint::black_box(())
                    })
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, handle_lookup_diagnostics);
criterion_main!(benches);
