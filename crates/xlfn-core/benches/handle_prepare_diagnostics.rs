use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn_core::benchmark_support::{HandlePrepareDiagnosticCase, HandlePrepareDiagnostics};

const WORKERS: [usize; 6] = [1, 2, 4, 8, 16, 32];
const ITERATIONS_PER_WORKER: usize = 10_000;

fn handle_prepare_diagnostics(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_prepare_diagnostics");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));

    for case in HandlePrepareDiagnosticCase::ALL {
        for workers in WORKERS {
            let bench = HandlePrepareDiagnostics::new(case, workers, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(bench.total_iterations() as u64));
            group.bench_with_input(
                BenchmarkId::new(case.name(), workers),
                &bench,
                |b, bench| {
                    b.iter(|| {
                        bench.run();
                        std::hint::black_box(())
                    })
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, handle_prepare_diagnostics);
criterion_main!(benches);
