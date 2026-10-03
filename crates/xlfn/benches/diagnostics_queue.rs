//! Production reporting and explicit opt-in subscriber costs.
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    DiagnosticBenchCase, DiagnosticBenchmark, benchmark_measurement_time,
};

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("diagnostics_report");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(32));
    for case in [
        DiagnosticBenchCase::Disabled,
        DiagnosticBenchCase::SmallError,
        DiagnosticBenchCase::FullQueue,
        DiagnosticBenchCase::LargeBurst,
        DiagnosticBenchCase::TraceSmall,
        DiagnosticBenchCase::TraceLarge,
    ] {
        group.bench_function(case.name(), |b| {
            if case.is_burst() {
                b.iter_batched_ref(
                    || DiagnosticBenchmark::new(case),
                    |fixture| fixture.run(32),
                    BatchSize::PerIteration,
                );
            } else {
                let fixture = DiagnosticBenchmark::new(case);
                b.iter(|| fixture.run(32));
            }
        });
    }
    group.finish();
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
