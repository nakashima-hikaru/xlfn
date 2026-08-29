//! Opt-in hotpath driver for the RTD publish and refresh paths.
//!
//! This example intentionally reuses the production-oriented benchmark
//! fixtures. It is a profiling workload, not a correctness test or a second
//! benchmark implementation.

use xlfn::benchmark_support::{
    RTD_REFRESH_SCALING_CASES, RtdPublishStringBenchmark, RtdRefreshScalingBenchmark,
    RtdRefreshScalingCase, RtdRefreshValueKind,
};

const REFRESH_ITERATIONS: usize = 32;
const PUBLISH_ITERATIONS: usize = 100_000;

#[hotpath::main]
fn main() {
    profile_number_dense();
    profile_string_dense();
    profile_number_sparse();
    profile_string_sparse();
    profile_string_repeated_same();
    profile_string_changing();
}

#[inline(never)]
fn profile_number_dense() {
    profile_refresh("dense", RtdRefreshValueKind::Number);
}

#[inline(never)]
fn profile_string_dense() {
    profile_refresh("dense", RtdRefreshValueKind::ShortString);
}

#[inline(never)]
fn profile_number_sparse() {
    profile_refresh("sparse", RtdRefreshValueKind::Number);
}

#[inline(never)]
fn profile_string_sparse() {
    profile_refresh("sparse", RtdRefreshValueKind::ShortString);
}

fn profile_refresh(name: &'static str, value_kind: RtdRefreshValueKind) {
    let case = refresh_case(name);
    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
    for _ in 0..REFRESH_ITERATIONS {
        benchmark.run_end_to_end_cycle();
    }
}

#[inline(never)]
fn profile_string_repeated_same() {
    let benchmark = RtdPublishStringBenchmark::new();
    benchmark.run_repeated_same(PUBLISH_ITERATIONS);
}

#[inline(never)]
fn profile_string_changing() {
    let benchmark = RtdPublishStringBenchmark::new();
    benchmark.run_changing(PUBLISH_ITERATIONS);
}

fn refresh_case(name: &'static str) -> RtdRefreshScalingCase {
    RTD_REFRESH_SCALING_CASES
        .iter()
        .copied()
        .find(|case| case.name == name)
        .expect("profiling workload must use a declared RTD scaling case")
}
