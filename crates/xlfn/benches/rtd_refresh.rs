use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RTD_REFRESH_SCALING_CASES, RtdRefreshScalingBenchmark, RtdRefreshValueKind,
    benchmark_measurement_time,
};

fn rtd_refresh_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_refresh");
    group.measurement_time(benchmark_measurement_time());

    let case_dense = RTD_REFRESH_SCALING_CASES
        .iter()
        .find(|c| c.name == "dense")
        .copied()
        .expect("dense scaling case must exist");

    let case_sparse = RTD_REFRESH_SCALING_CASES
        .iter()
        .find(|c| c.name == "sparse")
        .copied()
        .expect("sparse scaling case must exist");

    // Number pipelines
    let num_dense = RtdRefreshScalingBenchmark::new(case_dense, RtdRefreshValueKind::Number);
    group.throughput(Throughput::Elements(case_dense.updated_topics as u64));

    group.bench_function(BenchmarkId::new("number/collection", "dense"), |b| {
        b.iter_custom(|iterations| num_dense.measure_refresh_collection(iterations));
    });
    group.bench_function(BenchmarkId::new("number/reduction", "dense"), |b| {
        b.iter_custom(|iterations| num_dense.measure_refresh_reduction(iterations));
    });
    group.bench_function(BenchmarkId::new("number/completion", "dense"), |b| {
        b.iter_custom(|iterations| num_dense.measure_refresh_completion(iterations));
    });

    let num_sparse = RtdRefreshScalingBenchmark::new(case_sparse, RtdRefreshValueKind::Number);
    group.throughput(Throughput::Elements(case_sparse.updated_topics as u64));
    group.bench_function(BenchmarkId::new("number/end_to_end", "sparse"), |b| {
        b.iter(|| num_sparse.run_end_to_end_cycle());
    });

    group.throughput(Throughput::Elements(case_dense.updated_topics as u64));
    group.bench_function(BenchmarkId::new("number/end_to_end", "dense"), |b| {
        b.iter(|| num_dense.run_end_to_end_cycle());
    });

    // Short string end-to-end (dense)
    let short_dense = RtdRefreshScalingBenchmark::new(case_dense, RtdRefreshValueKind::ShortString);
    group.bench_function(BenchmarkId::new("short_string/end_to_end", "dense"), |b| {
        b.iter(|| short_dense.run_end_to_end_cycle());
    });

    // String 8 KiB regression suite (dense)
    let string_8k_dense =
        RtdRefreshScalingBenchmark::new(case_dense, RtdRefreshValueKind::String8KiB);
    group.bench_function(BenchmarkId::new("string_8k/collection", "dense"), |b| {
        b.iter_custom(|iterations| string_8k_dense.measure_refresh_collection(iterations));
    });
    group.bench_function(BenchmarkId::new("string_8k/completion", "dense"), |b| {
        b.iter_custom(|iterations| string_8k_dense.measure_refresh_completion(iterations));
    });
    group.bench_function(BenchmarkId::new("string_8k/end_to_end", "dense"), |b| {
        b.iter(|| string_8k_dense.run_end_to_end_cycle());
    });

    group.finish();
}

criterion_group!(benches, rtd_refresh_benchmarks);
criterion_main!(benches);
