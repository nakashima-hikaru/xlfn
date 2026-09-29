use criterion::{Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::{
    SemanticIdentityBenchmark, Utf16IdentityBenchmark, benchmark_measurement_time,
};
use xlfn::value::Matrix;

fn input_identity_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_identity");
    group.measurement_time(benchmark_measurement_time());

    for (name, text) in [
        ("ascii_short", "short".to_owned()),
        ("ascii_1k", "a".repeat(1_024)),
        ("unicode_short", "日本語💡".to_owned()),
        ("unicode_1k", "日本語💡".repeat(80)),
        ("unicode_limit", "あ".repeat(32_767)),
        ("unicode_sparse", format!("{}é", "a".repeat(1_024))),
    ] {
        let value = Utf16IdentityBenchmark::new(&text);
        group.bench_function(format!("utf16/{name}"), |b| {
            b.iter(|| std::hint::black_box(value.run()));
        });
    }

    let f64_value = SemanticIdentityBenchmark::new(42.0_f64);
    group.bench_function("f64", |b| {
        b.iter(|| std::hint::black_box(f64_value.run()));
    });

    let string_value = SemanticIdentityBenchmark::new(String::from("short"));
    group.bench_function("string_short", |b| {
        b.iter(|| std::hint::black_box(string_value.run()));
    });

    for cells in [16, 256, 4_096] {
        let matrix = Matrix::new(1, cells, (0..cells).map(|index| index as f64).collect())
            .expect("benchmark matrix dimensions must be valid");
        let value = SemanticIdentityBenchmark::new(matrix);
        group.bench_function(format!("matrix_f64_{cells}"), |b| {
            b.iter(|| std::hint::black_box(value.run()));
        });
        group.bench_function(format!("eight_matrix_f64_{cells}"), |b| {
            b.iter(|| std::hint::black_box(value.run_arguments(8)));
        });
    }

    let matrix = Matrix::new(10, 10_000, (0..100_000).map(|index| index as f64).collect())
        .expect("benchmark matrix dimensions must be valid");
    let matrix_value = SemanticIdentityBenchmark::new(matrix);
    group.bench_function("matrix_f64_100k", |b| {
        b.iter(|| std::hint::black_box(matrix_value.run()));
    });

    group.finish();
}

criterion_group!(benches, input_identity_benchmarks);
criterion_main!(benches);
