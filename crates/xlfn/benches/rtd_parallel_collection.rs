use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RTD_PARALLEL_CROSSING_CASES, RtdRefreshScalingBenchmark, RtdRefreshValueKind,
    benchmark_measurement_time,
};

fn rtd_parallel_collection_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_parallel_collection");
    group.measurement_time(benchmark_measurement_time());

    for (value_name, value_kind) in [
        ("number", RtdRefreshValueKind::Number),
        ("short_string", RtdRefreshValueKind::ShortString),
    ] {
        for case in RTD_PARALLEL_CROSSING_CASES {
            group.throughput(Throughput::Elements(case.updated_topics as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/collection/sequential"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_collection(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/collection/parallel"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    benchmark.assert_parallel_collection_equivalent();
                    b.iter_custom(|iterations| {
                        benchmark.measure_parallel_refresh_collection(iterations)
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(
                    format!("{value_name}/reduction/sequential_origin"),
                    case.name,
                ),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_reduction(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/reduction/parallel_origin"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    benchmark.assert_parallel_collection_equivalent();
                    b.iter_custom(|iterations| {
                        benchmark.measure_parallel_refresh_reduction(iterations)
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/end_to_end/sequential"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter(|| benchmark.run_end_to_end_cycle());
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/end_to_end/parallel"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    benchmark.assert_parallel_collection_equivalent();
                    b.iter(|| benchmark.run_parallel_end_to_end_cycle());
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, rtd_parallel_collection_benchmarks);
criterion_main!(benches);
