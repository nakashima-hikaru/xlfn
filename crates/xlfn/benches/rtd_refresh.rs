use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RTD_REFRESH_SCALING_CASES, RtdRefreshScalingBenchmark, RtdRefreshValueKind,
    benchmark_measurement_time,
};

fn rtd_refresh_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_refresh");
    group.measurement_time(benchmark_measurement_time());

    for (value_name, value_kind) in [
        ("number", RtdRefreshValueKind::Number),
        ("short_string", RtdRefreshValueKind::ShortString),
    ] {
        for case in RTD_REFRESH_SCALING_CASES {
            group.throughput(Throughput::Elements(case.updated_topics as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/publish_coalesce"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter(|| benchmark.publish_coalesced());
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/planning"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_planning(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/collection"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_collection(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/reduction"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_reduction(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/completion"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter_custom(|iterations| benchmark.measure_refresh_completion(iterations));
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{value_name}/end_to_end"), case.name),
                &case,
                |b, &case| {
                    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
                    b.iter(|| benchmark.run_end_to_end_cycle());
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, rtd_refresh_benchmarks);
criterion_main!(benches);
