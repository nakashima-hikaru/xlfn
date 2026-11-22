use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    RtdPublishNumberBenchmark, RtdPublishStringBenchmark, benchmark_measurement_time,
};

const ITERATIONS: usize = 10_000;

fn rtd_publish_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_publish");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(ITERATIONS as u64));

    group.bench_with_input(
        BenchmarkId::new("number", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("number", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishNumberBenchmark::new();
            b.iter(|| bench.run_repeated_same(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            let bench = RtdPublishStringBenchmark::new();
            b.iter(|| bench.run_repeated_same(iterations));
        },
    );

    let bench_8k = RtdPublishStringBenchmark::with_payload_len(8 * 1024);
    group.bench_with_input(
        BenchmarkId::new("string_8k", "changing"),
        &ITERATIONS,
        |b, &iterations| {
            b.iter(|| bench_8k.run_changing(iterations));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("string_8k", "same_value"),
        &ITERATIONS,
        |b, &iterations| {
            b.iter(|| bench_8k.run_repeated_same(iterations));
        },
    );

    for (name, bench) in [
        ("string_borrowed", RtdPublishStringBenchmark::new()),
        ("string_8k_borrowed", bench_8k),
    ] {
        group.bench_with_input(
            BenchmarkId::new(name, "changing"),
            &ITERATIONS,
            |b, &iterations| {
                b.iter(|| bench.run_borrowed_changing(iterations));
            },
        );
        group.bench_with_input(
            BenchmarkId::new(name, "same_value"),
            &ITERATIONS,
            |b, &iterations| {
                b.iter(|| bench.run_borrowed_repeated_same(iterations));
            },
        );
    }

    for producers in [1, 4, 16, 32] {
        for capacity in [64, 1024] {
            let mut pipeline =
                xlfn::benchmark_support::RtdChannelPipelineBenchmark::new(capacity, producers);
            group.throughput(Throughput::Elements((producers * 10_000) as u64));
            group.bench_function(
                BenchmarkId::new("channel_pipeline", format!("p{producers}_c{capacity}")),
                |b| {
                    b.iter(|| pipeline.run_cycle(10_000));
                },
            );
        }
    }
    group.finish();

    const MIXED_ITERATIONS: usize = 1_000;
    let mut group = c.benchmark_group("rtd_publish_string_mix");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(MIXED_ITERATIONS as u64));
    for (length, bench) in [
        ("short", RtdPublishStringBenchmark::new()),
        ("8k", RtdPublishStringBenchmark::with_payload_len(8 * 1024)),
    ] {
        for duplicate_percent in [0, 50, 99, 100] {
            let payloads = bench.mixed_payloads(duplicate_percent, MIXED_ITERATIONS);
            let case = format!("{length}/{duplicate_percent}%_duplicate");

            group.bench_function(BenchmarkId::new("moved", &case), |b| {
                b.iter_batched(
                    || {
                        bench.run_borrowed_repeated_same(1);
                        payloads
                            .iter()
                            .map(|text| (*text).to_owned())
                            .collect::<Vec<_>>()
                    },
                    |owned| bench.run_owned_payloads(owned),
                    BatchSize::PerIteration,
                );
            });

            group.bench_function(BenchmarkId::new("cloned", &case), |b| {
                b.iter_batched(
                    || bench.run_borrowed_repeated_same(1),
                    |()| bench.run_owned_payloads(payloads.iter().map(|text| (*text).to_owned())),
                    BatchSize::PerIteration,
                );
            });

            group.bench_function(BenchmarkId::new("borrowed", &case), |b| {
                b.iter_batched(
                    || bench.run_borrowed_repeated_same(1),
                    |()| bench.run_borrowed_payloads(&payloads),
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, rtd_publish_benchmarks);
criterion_main!(benches);
