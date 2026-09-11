//! Deterministic recalculation traces with real weighted payloads and miss work.
//! Run one backend per process via XLFN_CACHE_BACKEND; output is JSONL.
use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;
use xlfn::benchmark_support::benchmark_cache_backend;
use xlfn::unstable::cache::CalculationCache;

const KIB: usize = 1024;

struct Trace {
    name: &'static str,
    budget: usize,
    keys: Vec<u64>,
    weight: fn(u64) -> usize,
}

fn mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn main() {
    let backend = benchmark_cache_backend();
    let traces = [
        Trace {
            name: "fits_half_budget",
            budget: 1024 * KIB,
            keys: (0..4096).map(|index| index % 64).collect(),
            weight: |_| 8 * KIB,
        },
        Trace {
            name: "fits_near_budget",
            budget: 1024 * KIB,
            keys: (0..4800).map(|index| index % 120).collect(),
            weight: |_| 8 * KIB,
        },
        Trace {
            name: "interleaved_scan",
            budget: 512 * KIB,
            keys: (0..8192)
                .map(|index| {
                    if index % 4 == 0 {
                        1024 + index
                    } else {
                        mix(index) % 32
                    }
                })
                .collect(),
            weight: |_| 8 * KIB,
        },
        Trace {
            name: "changing_working_set",
            budget: 256 * KIB,
            keys: (0..8192)
                .map(|index| (index / 512) * 32 + index % 32)
                .collect(),
            weight: |_| 8 * KIB,
        },
        Trace {
            name: "one_large_matrix",
            budget: 1024 * KIB,
            keys: vec![0; 128],
            weight: |_| 768 * KIB,
        },
        Trace {
            name: "large_and_small_matrices",
            budget: 1024 * KIB,
            keys: (0..512)
                .map(|index| {
                    if index % 2 == 0 {
                        0
                    } else {
                        1 + (index / 2) % 32
                    }
                })
                .collect(),
            weight: |key| if key == 0 { 512 * KIB } else { 8 * KIB },
        },
        Trace {
            name: "variable_weight_pressure",
            budget: 1024 * KIB,
            keys: (0..1024).map(|index| mix(index) % 32).collect(),
            weight: |key| (1 + key as usize % 16) * 16 * KIB,
        },
    ];

    for trace in traces {
        let cache = CalculationCache::<u64, Box<[u64]>>::new_with_backend(trace.budget, backend);
        for phase in ["cold", "repeat"] {
            let computes = Cell::new(0_usize);
            let computed_bytes = Cell::new(0_usize);
            let started = Instant::now();
            for &key in &trace.keys {
                let lease = cache
                    .get_or_try_insert_with(
                        key,
                        |payload| payload.len() * size_of::<u64>(),
                        || {
                            computes.set(computes.get() + 1);
                            let bytes = (trace.weight)(key);
                            computed_bytes.set(computed_bytes.get() + bytes);
                            let payload = (0..bytes / size_of::<u64>())
                                .map(|index| mix(black_box(key) ^ index as u64))
                                .collect::<Vec<_>>()
                                .into_boxed_slice();
                            Ok(payload)
                        },
                    )
                    .unwrap();
                black_box(&*lease);
            }
            let elapsed = started.elapsed();
            cache.maintenance();
            let resident = cache.resident_stats();
            let debt = cache.reclamation_stats();
            assert!(resident.weight <= trace.budget as u64);
            println!(
                "{}",
                serde_json::json!({
                    "backend": format!("{backend:?}"), "trace": trace.name, "phase": phase,
                    "requests": trace.keys.len(), "computes": computes.get(),
                    "hit_rate": 1.0 - computes.get() as f64 / trace.keys.len() as f64,
                    "computed_payload_bytes": computed_bytes.get(),
                    "elapsed_ns": elapsed.as_nanos(), "budget_bytes": trace.budget,
                    "resident_weight": resident.weight, "resident_entries": resident.entries,
                    "index_bytes_estimate": resident.index_bytes_estimate,
                    "index_metadata_opaque": resident.index_metadata_opaque,
                    "peak_pending_nodes": debt.peak_pending_nodes,
                    "peak_pending_weight": debt.peak_pending_weight,
                    "pending_nodes": debt.pending_nodes,
                })
            );
        }
        cache.clear();
        cache.maintenance();
        assert_eq!(cache.reclamation_stats().pending_nodes, 0);
    }
}
