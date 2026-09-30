use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    ObjectFinalPinRelease, ObjectLeaseBenchCase, ObjectLeaseBenchmark, benchmark_measurement_time,
};

const ITERATIONS_PER_WORKER: usize = 1_000;
const WORKERS: [usize; 3] = [1, 4, 16];

fn object_lease_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_lease");
    group.measurement_time(benchmark_measurement_time());
    let serial = ObjectLeaseBenchmark::new(ObjectLeaseBenchCase::SameObject, 1, 1);
    group.throughput(Throughput::Elements(1));
    group.bench_function("pin_acquire_release_serial", |b| {
        b.iter(|| serial.run_serial())
    });
    group.bench_function("final_pin_release", |b| {
        b.iter_batched(
            ObjectFinalPinRelease::prepare,
            ObjectFinalPinRelease::release,
            BatchSize::NumIterations(1_024),
        );
    });
    for case in ObjectLeaseBenchCase::ALL {
        for workers in WORKERS {
            let benchmark = ObjectLeaseBenchmark::new(case, workers, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
            group.bench_with_input(BenchmarkId::new(case.name(), workers), &workers, |b, _| {
                b.iter(|| benchmark.run());
            });
        }
    }
    group.finish();
}

criterion_group!(benches, object_lease_benchmarks);
criterion_main!(benches);
