use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    FormulaCallerBenchCase, FormulaCallerBenchmark, FormulaCallerWorkerPool,
    benchmark_measurement_time,
};

fn formula_caller_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve_formula_caller");
    group.measurement_time(benchmark_measurement_time());

    for case in [FormulaCallerBenchCase::Ref, FormulaCallerBenchCase::SRef] {
        let benchmark = FormulaCallerBenchmark::new(case);
        group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
            b.iter(|| std::hint::black_box(benchmark.run()));
        });
    }

    group.finish();

    const ITERATIONS: usize = 1_000;
    let mut group = c.benchmark_group("resolve_formula_caller/concurrent");
    group.measurement_time(benchmark_measurement_time());
    for workers in [1, 4, 16, 32] {
        group.throughput(Throughput::Elements((workers * ITERATIONS) as u64));
        for case in [FormulaCallerBenchCase::Ref, FormulaCallerBenchCase::SRef] {
            let pool = FormulaCallerWorkerPool::new(workers, ITERATIONS, case);
            group.bench_function(BenchmarkId::new(case.name(), workers), |b| {
                b.iter(|| pool.run_batch());
            });
        }
        let dispatch = FormulaCallerWorkerPool::dispatch_only(workers);
        group.bench_function(BenchmarkId::new("dispatch_only", workers), |b| {
            b.iter(|| dispatch.run_batch());
        });
    }
    group.finish();
}

criterion_group!(benches, formula_caller_benchmarks);
criterion_main!(benches);
