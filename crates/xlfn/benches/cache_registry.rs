use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use xlfn::benchmark_support::{RegistryCacheBenchmark, benchmark_measurement_time};
use xlfn::cache::{CacheEndpoint, CacheRegistry, CalculationCache};

const ITERATIONS_PER_WORKER: usize = 10_000;

fn cache_registry_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_registry");
    group.measurement_time(benchmark_measurement_time());

    for workers in [1, 8, 32] {
        let benchmark = RegistryCacheBenchmark::distinct_endpoints(workers, ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("distinct_endpoints", workers), |b| {
            b.iter(|| benchmark.run());
        });
    }

    for endpoints in [1, 8, 16] {
        let benchmark = RegistryCacheBenchmark::endpoint_cycle(endpoints, ITERATIONS_PER_WORKER);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("endpoint_cycle", endpoints), |b| {
            b.iter(|| benchmark.run());
        });
    }

    for workers in [1, 8, 32] {
        for endpoints in [16, 64, 256] {
            let benchmark =
                RegistryCacheBenchmark::endpoint_cycles(workers, endpoints, ITERATIONS_PER_WORKER);
            group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
            group.bench_function(
                BenchmarkId::new(format!("endpoint_cycles/{endpoints}"), workers),
                |b| b.iter(|| benchmark.run()),
            );
        }
    }

    for markers in [3, 8] {
        let iterations = ITERATIONS_PER_WORKER / markers * markers;
        let benchmark = RegistryCacheBenchmark::same_id_marker_cycle(markers, iterations);
        group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
        group.bench_function(BenchmarkId::new("same_id_marker_cycle", markers), |b| {
            b.iter(|| benchmark.run());
        });
    }

    let benchmark = RegistryCacheBenchmark::same_address_prefix_cycle(ITERATIONS_PER_WORKER);
    group.throughput(Throughput::Elements(benchmark.total_iterations() as u64));
    group.bench_function(BenchmarkId::new("same_address_prefix_cycle", 3), |b| {
        b.iter(|| benchmark.run());
    });

    group.finish();

    let mut group = c.benchmark_group("cache_borrowed_keys");
    group.measurement_time(benchmark_measurement_time());
    group.throughput(Throughput::Elements(1));
    for length in [16, 1024] {
        let key = "k".repeat(length);
        let cache = CalculationCache::<String, u64>::new(8);
        drop(
            cache
                .get_or_try_insert_borrowed(key.as_str(), || Ok(7))
                .unwrap(),
        );
        group.bench_function(
            BenchmarkId::new("calculation_owned_warm_hit", length),
            |b| {
                b.iter(|| {
                    black_box(
                        cache
                            .get_or_try_insert_with(black_box(&key).clone(), || panic!("warm hit"))
                            .unwrap(),
                    );
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("calculation_borrowed_warm_hit", length),
            |b| {
                b.iter(|| {
                    black_box(
                        cache
                            .get_or_try_insert_borrowed(black_box(key.as_str()), || {
                                panic!("warm hit")
                            })
                            .unwrap(),
                    );
                });
            },
        );
        let registry = CacheRegistry::new(8);
        let endpoint = CacheEndpoint::<String, u64>::new("borrowed_keys");
        drop(
            registry
                .get_or_try_insert_borrowed(&endpoint, key.as_str(), || Ok(7))
                .unwrap(),
        );
        group.bench_function(BenchmarkId::new("registry_owned_warm_hit", length), |b| {
            b.iter(|| {
                black_box(
                    registry
                        .get_or_try_insert(&endpoint, black_box(&key).clone(), || {
                            panic!("warm hit")
                        })
                        .unwrap(),
                );
            });
        });
        group.bench_function(
            BenchmarkId::new("registry_borrowed_warm_hit", length),
            |b| {
                b.iter(|| {
                    black_box(
                        registry
                            .get_or_try_insert_borrowed(&endpoint, black_box(key.as_str()), || {
                                panic!("warm hit")
                            })
                            .unwrap(),
                    );
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, cache_registry_benchmarks);
criterion_main!(benches);
