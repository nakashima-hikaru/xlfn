//! Uninstrumented timing, separate from the counting allocator executable.
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    ObjectPayloadKind, ObjectStorageBenchmark, benchmark_measurement_time,
};

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_object_storage");
    group.measurement_time(benchmark_measurement_time());
    for objects in [1, 256] {
        for aliases in [1, 32] {
            group.throughput(Throughput::Elements(objects as u64));
            for kind in ObjectPayloadKind::ALL {
                let fixture = ObjectStorageBenchmark::new(objects, aliases);
                fixture.run_cycle(kind);
                group.bench_function(
                    BenchmarkId::new(kind.name(), format!("o{objects}_a{aliases}")),
                    |b| b.iter(|| fixture.run_cycle(kind)),
                );
            }
        }
    }
    group.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
