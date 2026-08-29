use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RTD_STRING_REPRESENTATION_LENGTHS, RtdStringRepresentationBenchmark,
};

fn rtd_string_representation_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_string_representation");
    group.measurement_time(xlfn::benchmark_support::benchmark_measurement_time());

    for &length in &RTD_STRING_REPRESENTATION_LENGTHS {
        let benchmark = RtdStringRepresentationBenchmark::new(length);
        let payload = benchmark.payload();

        group.bench_with_input(
            BenchmarkId::new("std_arc_str_from_string", length),
            &payload,
            |b, payload| {
                b.iter_batched(
                    || (*payload).to_owned(),
                    RtdStringRepresentationBenchmark::convert_std_arc_str,
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("std_arc_string", length),
            &payload,
            |b, payload| {
                b.iter_batched(
                    || (*payload).to_owned(),
                    RtdStringRepresentationBenchmark::convert_std_arc_string,
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("triomphe_arc_string", length),
            &payload,
            |b, payload| {
                b.iter_batched(
                    || (*payload).to_owned(),
                    RtdStringRepresentationBenchmark::convert_triomphe_arc_string,
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, rtd_string_representation_benchmarks);
criterion_main!(benches);
