use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use xlfn::benchmark_support::{BenchHandleObject, RawArgumentIngressBenchmark};
use xlfn::value::{ExcelCellValue, ExcelValue, Matrix};

fn argument_ingress_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("argument_ingress");
    group.measurement_time(Duration::from_secs(10));

    // 1. Scalar f64
    let mut f64_bench = RawArgumentIngressBenchmark::number(42.0);
    group.bench_function("f64/plain", |b| {
        b.iter(|| f64_bench.run_plain::<f64>());
    });
    group.bench_function("f64/with_identity", |b| {
        b.iter(|| f64_bench.run_with_identity::<f64>());
    });

    // 2. Short string
    let mut str_short = RawArgumentIngressBenchmark::string("short_str");
    group.bench_function("string_short/plain", |b| {
        b.iter(|| str_short.run_plain::<String>());
    });
    group.bench_function("string_short/with_identity", |b| {
        b.iter(|| str_short.run_with_identity::<String>());
    });

    // 3. Long string
    let long_text = "a".repeat(1000);
    let mut str_long = RawArgumentIngressBenchmark::string(&long_text);
    group.bench_function("string_1k/plain", |b| {
        b.iter(|| str_long.run_plain::<String>());
    });
    group.bench_function("string_1k/with_identity", |b| {
        b.iter(|| str_long.run_with_identity::<String>());
    });

    // 4. Matrix<f64> 100k
    let mut mat_100k = RawArgumentIngressBenchmark::number_matrix(10, 10_000);
    group.bench_function("matrix_f64_100k/plain", |b| {
        b.iter(|| mat_100k.run_plain::<Matrix<f64>>());
    });
    group.bench_function("matrix_f64_100k/with_identity", |b| {
        b.iter(|| mat_100k.run_with_identity::<Matrix<f64>>());
    });

    // 5. Vec<f64> 100k
    let mut vec_100k = RawArgumentIngressBenchmark::number_vec(100_000);
    group.bench_function("vec_f64_100k/plain", |b| {
        b.iter(|| vec_100k.run_plain::<Vec<f64>>());
    });
    group.bench_function("vec_f64_100k/with_identity", |b| {
        b.iter(|| vec_100k.run_with_identity::<Vec<f64>>());
    });

    // 6. ExcelCellValue
    let mut cell_num = RawArgumentIngressBenchmark::number(42.0);
    group.bench_function("cell_value_number/plain", |b| {
        b.iter(|| cell_num.run_plain::<ExcelCellValue>());
    });
    group.bench_function("cell_value_number/with_identity", |b| {
        b.iter(|| cell_num.run_with_identity::<ExcelCellValue>());
    });

    // 7. ExcelValue
    let mut val_scalar = RawArgumentIngressBenchmark::number(42.0);
    group.bench_function("excel_value_scalar/plain", |b| {
        b.iter(|| val_scalar.run_plain::<ExcelValue>());
    });
    group.bench_function("excel_value_scalar/with_identity", |b| {
        b.iter(|| val_scalar.run_with_identity::<ExcelValue>());
    });

    let mut val_arr = RawArgumentIngressBenchmark::number_matrix(10, 10_000);
    group.bench_function("excel_value_matrix_100k/plain", |b| {
        b.iter(|| val_arr.run_plain::<ExcelValue>());
    });
    group.bench_function("excel_value_matrix_100k/with_identity", |b| {
        b.iter(|| val_arr.run_with_identity::<ExcelValue>());
    });

    // 8. Handle
    let mut handle_bench = RawArgumentIngressBenchmark::handle();
    group.bench_function("handle/plain", |b| {
        b.iter(|| handle_bench.run_handle_plain::<BenchHandleObject>());
    });
    group.bench_function("handle/with_identity", |b| {
        b.iter(|| handle_bench.run_handle_with_identity::<BenchHandleObject>());
    });

    group.finish();
}

criterion_group!(benches, argument_ingress_benchmarks);
criterion_main!(benches);
