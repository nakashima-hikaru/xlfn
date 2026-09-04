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
const DEFAULT_ITERATION_MULTIPLIER: usize = 1;

#[hotpath::main]
fn main() {
    let mut args = std::env::args().skip(1);
    let scenario = args.next();
    let multiplier = parse_multiplier(args.next().as_deref());
    if args.next().is_some() {
        panic!("too many RTD profiling arguments; expected SCENARIO [ITERATION_MULTIPLIER]");
    }

    match scenario.as_deref() {
        Some("number-dense") => profile_number_dense(multiplier),
        Some("string-dense") => profile_string_dense(multiplier),
        Some("number-sparse") => profile_number_sparse(multiplier),
        Some("string-sparse") => profile_string_sparse(multiplier),
        Some("string-same") => profile_string_repeated_same(multiplier),
        Some("string-changing") => profile_string_changing(multiplier),
        _ => panic!(
            "unknown RTD profiling scenario; expected one of: \
             number-dense, string-dense, number-sparse, string-sparse, string-same, \
             string-changing"
        ),
    }
}

fn parse_multiplier(raw: Option<&str>) -> usize {
    let Some(raw) = raw else {
        return DEFAULT_ITERATION_MULTIPLIER;
    };
    let multiplier = raw
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("iteration multiplier must be a positive integer: {raw}"));
    assert!(multiplier > 0, "iteration multiplier must be positive");
    multiplier
}

fn scaled_iterations(base: usize, multiplier: usize) -> usize {
    base.checked_mul(multiplier)
        .expect("iteration multiplier overflowed the workload size")
}

#[inline(never)]
fn profile_number_dense(multiplier: usize) {
    profile_refresh("dense", RtdRefreshValueKind::Number, multiplier);
}

#[inline(never)]
fn profile_string_dense(multiplier: usize) {
    profile_refresh("dense", RtdRefreshValueKind::ShortString, multiplier);
}

#[inline(never)]
fn profile_number_sparse(multiplier: usize) {
    profile_refresh("sparse", RtdRefreshValueKind::Number, multiplier);
}

#[inline(never)]
fn profile_string_sparse(multiplier: usize) {
    profile_refresh("sparse", RtdRefreshValueKind::ShortString, multiplier);
}

fn profile_refresh(name: &'static str, value_kind: RtdRefreshValueKind, multiplier: usize) {
    let case = refresh_case(name);
    let benchmark = RtdRefreshScalingBenchmark::new(case, value_kind);
    for _ in 0..scaled_iterations(REFRESH_ITERATIONS, multiplier) {
        benchmark.run_end_to_end_cycle();
    }
}

#[inline(never)]
fn profile_string_repeated_same(multiplier: usize) {
    let benchmark = RtdPublishStringBenchmark::new();
    benchmark.run_repeated_same(scaled_iterations(PUBLISH_ITERATIONS, multiplier));
}

#[inline(never)]
fn profile_string_changing(multiplier: usize) {
    let benchmark = RtdPublishStringBenchmark::new();
    benchmark.run_changing(scaled_iterations(PUBLISH_ITERATIONS, multiplier));
}

fn refresh_case(name: &'static str) -> RtdRefreshScalingCase {
    RTD_REFRESH_SCALING_CASES
        .iter()
        .copied()
        .find(|case| case.name == name)
        .expect("profiling workload must use a declared RTD scaling case")
}
