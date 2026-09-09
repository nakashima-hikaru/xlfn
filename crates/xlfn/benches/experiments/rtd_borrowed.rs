use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{RtdBorrowedBenchmark, benchmark_measurement_time};

fn cases<const N: usize, const BORROWED: bool>(c: &mut Criterion) {
    let mut group = c.benchmark_group(if BORROWED {
        "rtd_borrowed"
    } else {
        "rtd_owned_e2e"
    });
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(256));
    for operation in ["new", "existing", "churn"] {
        group.bench_function(BenchmarkId::new(operation, N), |b| {
            b.iter_batched_ref(
                || RtdBorrowedBenchmark::<N>::new(256, operation == "existing"),
                |bench| bench.run::<BORROWED>(operation == "churn"),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}
fn benchmarks(c: &mut Criterion) {
    cases::<1, false>(c);
    cases::<10, false>(c);
    cases::<1, true>(c);
    cases::<10, true>(c);
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
