use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use xlfn::{benchmark_support::benchmark_measurement_time, rtd::RtdTopic};

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtd_topic_storage");
    group.measurement_time(benchmark_measurement_time());
    for (name, part) in [
        ("ascii8", "x".repeat(8)),
        ("ascii23", "x".repeat(23)),
        ("ascii24", "x".repeat(24)),
        ("ascii128", "x".repeat(128)),
        ("unicode21", "漢".repeat(7)),
        ("unicode24", "漢".repeat(8)),
        ("unicode1536", "漢".repeat(512)),
    ] {
        for count in [1, 10] {
            let parts = vec![part.clone(); count];
            let case = format!("{name}/{count}");
            group.bench_function(BenchmarkId::new("borrowed", &case), |b| {
                b.iter(|| {
                    black_box(RtdTopic::new(black_box(&parts).iter().map(String::as_str)).unwrap())
                });
            });
            group.bench_function(BenchmarkId::new("owned", &case), |b| {
                b.iter_batched(
                    || parts.clone(),
                    |parts| black_box(RtdTopic::new(black_box(parts)).unwrap()),
                    BatchSize::SmallInput,
                );
            });
            let topic = RtdTopic::new(parts.iter().map(String::as_str)).unwrap();
            group.bench_function(BenchmarkId::new("clone", &case), |b| {
                b.iter(|| black_box(black_box(&topic).clone()));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
