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
    match std::env::args().nth(1).as_deref() {
        Some("number-dense") => profile_number_dense(),
        Some("string-dense") => profile_string_dense(),
        Some("number-sparse") => profile_number_sparse(),
        Some("string-sparse") => profile_string_sparse(),
        Some("string-same") => profile_string_repeated_same(),
        Some("string-changing") => profile_string_changing(),
        Some("string-stored") => profile_string_stored(),
        _ => panic!(
            "unknown RTD profiling scenario; expected one of: \
             number-dense, string-dense, number-sparse, string-sparse, \
             string-same, string-changing, string-stored"
        ),
    }
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

#[inline(never)]
fn profile_string_stored() {
    let benchmark = RtdPublishStringBenchmark::new();
    benchmark.run_stored_publish(PUBLISH_ITERATIONS);
}

fn refresh_case(name: &'static str) -> RtdRefreshScalingCase {
    RTD_REFRESH_SCALING_CASES
        .iter()
        .copied()
        .find(|case| case.name == name)
        .expect("profiling workload must use a declared RTD scaling case")
}
