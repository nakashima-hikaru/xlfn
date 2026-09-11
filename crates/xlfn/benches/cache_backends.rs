//! Supplemental resident-backend qualification. JSONL on stdout, no Criterion.
//! Throughput and instrumented latency are measured in separate batches.
use std::hint::black_box;
use std::sync::{Arc, Barrier, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use xlfn::unstable::cache::{CacheBackend, CacheLease, CalculationCache};

const OPS: usize = 256;
const BUDGET: usize = 64 * 8;

#[derive(Clone, Copy, Debug)]
enum Workload {
    Hot,
    Disjoint,
    Mixed,
    Eviction,
    LiveEviction,
    Invalidate,
    Clear,
}

#[derive(Default)]
struct Report {
    hits: usize,
    reads: usize,
    hit_ns: Vec<u64>,
    write_ns: Vec<u64>,
    sampled_peak_nodes: usize,
    sampled_peak_weight: u64,
    held: usize,
}

fn batch<'a, const LATENCY: bool>(
    cache: &'a CalculationCache<u64, u64>,
    workload: Workload,
    worker: usize,
    workers: usize,
    next: &mut u64,
    held: &mut [Option<CacheLease<'a, u64>>],
) -> Report {
    let mut report = Report::default();
    if LATENCY {
        report.hit_ns.reserve(OPS);
        report.write_ns.reserve(OPS);
    }
    for op in 0..OPS {
        let write = match workload {
            Workload::Hot | Workload::Disjoint => false,
            Workload::Mixed => op % 32 == 0,
            _ => true,
        };
        let started = LATENCY.then(Instant::now);
        if write {
            let key = match workload {
                Workload::Invalidate | Workload::Clear => worker as u64,
                _ => {
                    let key = *next;
                    *next += workers as u64;
                    key
                }
            };
            let lease = cache
                .get_or_try_insert_with(key, |_| 8, || Ok(key))
                .unwrap();
            black_box(*lease);
            if held.is_empty() {
                drop(lease);
            } else {
                drop(held[op % 32].replace(lease));
            }
            match workload {
                Workload::Invalidate => cache.invalidate(&key),
                Workload::Clear => cache.clear(),
                _ => (),
            }
        } else {
            let key = if matches!(workload, Workload::Disjoint) {
                worker as u64
            } else {
                0
            };
            let value = cache.get(&key);
            let hit = value.is_some();
            black_box(value);
            if LATENCY {
                report.reads += 1;
                report.hits += usize::from(hit);
                if hit {
                    report
                        .hit_ns
                        .push(started.unwrap().elapsed().as_nanos() as u64);
                }
            }
            // Restore an evicted hot key, but count the miss and include the
            // refill in throughput. Hit latency contains successful gets only.
            if !hit {
                drop(
                    cache
                        .get_or_try_insert_with(key, |_| 8, || Ok(key))
                        .unwrap(),
                );
            }
        }
        if LATENCY {
            if write {
                report
                    .write_ns
                    .push(started.unwrap().elapsed().as_nanos() as u64);
            }
            let stats = cache.reclamation_stats();
            report.sampled_peak_nodes = report.sampled_peak_nodes.max(stats.pending_nodes);
            report.sampled_peak_weight = report.sampled_peak_weight.max(stats.pending_weight);
        }
    }
    report.held = held.iter().filter(|lease| lease.is_some()).count();
    report
}

struct Pool {
    cache: Arc<CalculationCache<u64, u64>>,
    start: Vec<mpsc::SyncSender<bool>>,
    done: mpsc::Receiver<Report>,
    barrier: Arc<Barrier>,
    threads: Vec<JoinHandle<()>>,
}
impl Pool {
    fn new(backend: CacheBackend, workload: Workload, workers: usize) -> Self {
        let cache = Arc::new(CalculationCache::new_with_backend(BUDGET, backend));
        for key in 0..workers as u64 {
            drop(
                cache
                    .get_or_try_insert_with(key, |_| 8, || Ok(key))
                    .unwrap(),
            );
        }
        cache.maintenance();
        let barrier = Arc::new(Barrier::new(workers + 1));
        let (tx, done) = mpsc::sync_channel(workers);
        let mut start = Vec::new();
        let mut threads = Vec::new();
        for worker in 0..workers {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            start.push(sender);
            threads.push(thread::spawn(move || {
                let mut held = Vec::new();
                if matches!(workload, Workload::LiveEviction) {
                    held.resize_with(32, || None);
                }
                let mut next = 1024 + worker as u64;
                barrier.wait();
                while let Ok(latency) = receiver.recv() {
                    barrier.wait();
                    let report = if latency {
                        batch::<true>(&cache, workload, worker, workers, &mut next, &mut held)
                    } else {
                        batch::<false>(&cache, workload, worker, workers, &mut next, &mut held)
                    };
                    tx.send(report).unwrap();
                }
            }));
        }
        barrier.wait();
        Self {
            cache,
            start,
            done,
            barrier,
            threads,
        }
    }
    fn run(&self, latency: bool) -> Report {
        for start in &self.start {
            start.send(latency).unwrap();
        }
        self.barrier.wait();
        let mut total = Report::default();
        for _ in &self.threads {
            let report = self.done.recv().unwrap();
            total.hits += report.hits;
            total.reads += report.reads;
            total.hit_ns.extend(report.hit_ns);
            total.write_ns.extend(report.write_ns);
            total.sampled_peak_nodes = total.sampled_peak_nodes.max(report.sampled_peak_nodes);
            total.sampled_peak_weight = total.sampled_peak_weight.max(report.sampled_peak_weight);
            total.held += report.held;
        }
        total
    }
    fn stop(&mut self) {
        self.start.clear();
        for worker in self.threads.drain(..) {
            worker.join().unwrap();
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.stop();
    }
}

fn percentiles(values: &mut [u64]) -> serde_json::Value {
    values.sort_unstable();
    let at = |p: usize| {
        values
            .get((values.len() * p).div_ceil(100).saturating_sub(1))
            .copied()
    };
    serde_json::json!({ "samples": values.len(), "p50_ns": at(50), "p95_ns": at(95), "p99_ns": at(99) })
}

fn main() {
    let smoke = std::env::var_os("XLFN_BACKEND_SMOKE").is_some();
    let duration = Duration::from_millis(if smoke { 10 } else { 1000 });
    let mut backends = vec![
        CacheBackend::Moka,
        CacheBackend::Sharded { shards: 8 },
        CacheBackend::Sharded { shards: 16 },
        CacheBackend::Sharded { shards: 32 },
        CacheBackend::Sharded { shards: 64 },
        CacheBackend::QuickCache { shards: 1 },
        CacheBackend::QuickCache { shards: 8 },
        CacheBackend::QuickCache { shards: 32 },
    ];
    if let Ok(selected) = std::env::var("XLFN_CACHE_BACKENDS") {
        backends.retain(|backend| {
            let name = match backend {
                CacheBackend::Moka => "moka".to_owned(),
                CacheBackend::Sharded { shards } => format!("sharded{shards}"),
                CacheBackend::QuickCache { shards } => format!("quick{shards}"),
            };
            selected.split(',').any(|selected| selected == name)
        });
        assert!(!backends.is_empty(), "no selected cache backend");
    }
    let cases = [
        (Workload::Hot, 1),
        (Workload::Hot, 8),
        (Workload::Hot, 32),
        (Workload::Disjoint, 8),
        (Workload::Disjoint, 32),
        (Workload::Mixed, 1),
        (Workload::Mixed, 32),
        (Workload::Eviction, 1),
        (Workload::Eviction, 32),
        (Workload::LiveEviction, 1),
        (Workload::LiveEviction, 32),
        (Workload::Invalidate, 1),
        (Workload::Invalidate, 32),
        (Workload::Clear, 1),
    ];
    for repetition in 0..if smoke { 1 } else { 3 } {
        for offset in 0..backends.len() {
            let backend = backends[(offset + repetition) % backends.len()];
            for (workload, workers) in cases {
                let mut pool = Pool::new(backend, workload, workers);
                let warm = Instant::now();
                while warm.elapsed() < Duration::from_millis(if smoke { 1 } else { 200 }) {
                    pool.run(false);
                }
                let started = Instant::now();
                let mut batches = 0_u64;
                while started.elapsed() < duration {
                    pool.run(false);
                    batches += 1;
                }
                let seconds = started.elapsed().as_secs_f64();
                let mut probe = Report::default();
                for _ in 0..3 {
                    let report = pool.run(true);
                    probe.hits += report.hits;
                    probe.reads += report.reads;
                    probe.hit_ns.extend(report.hit_ns);
                    probe.write_ns.extend(report.write_ns);
                    probe.sampled_peak_nodes =
                        probe.sampled_peak_nodes.max(report.sampled_peak_nodes);
                    probe.sampled_peak_weight =
                        probe.sampled_peak_weight.max(report.sampled_peak_weight);
                    probe.held = report.held;
                }
                pool.cache.maintenance();
                let resident = pool.cache.resident_stats();
                assert!(resident.weight <= BUDGET as u64);
                let debt = pool.cache.reclamation_stats();
                pool.stop();
                pool.cache.clear();
                pool.cache.maintenance();
                let drained = pool.cache.reclamation_stats();
                assert_eq!((drained.pending_nodes, drained.pending_weight), (0, 0));
                println!(
                    "{}",
                    serde_json::json!({
                        "backend": format!("{backend:?}"), "workload": format!("{workload:?}"),
                        "workers": workers, "repetition": repetition, "smoke": smoke,
                        "batches": batches, "seconds": seconds,
                        "ops_per_second": batches as f64 * (workers * OPS) as f64 / seconds,
                        "hit_rate": if probe.reads == 0 { None } else { Some(probe.hits as f64 / probe.reads as f64) },
                        "hit_latency": percentiles(&mut probe.hit_ns),
                        "writer_latency": percentiles(&mut probe.write_ns),
                        "resident_entries": resident.entries, "resident_weight": resident.weight,
                        "index_bytes_estimate": resident.index_bytes_estimate,
                        "index_metadata_opaque": resident.index_metadata_opaque,
                        "resident_node_bytes_estimate": resident.resident_node_bytes_estimate,
                        "held_leases": probe.held, "held_payload_bytes_upper_bound": probe.held * 8,
                        "peak_pending_nodes": debt.peak_pending_nodes,
                        "peak_pending_weight": debt.peak_pending_weight,
                        "sampled_peak_pending_nodes": probe.sampled_peak_nodes,
                        "sampled_peak_pending_weight": probe.sampled_peak_weight,
                        "pending_nodes_after_drain": drained.pending_nodes,
                        "pending_weight_after_drain": drained.pending_weight,
                    })
                );
            }
            println!(
                "{}",
                serde_json::json!({
                    "backend": format!("{backend:?}"), "workload": "ControlledDebt", "workers": 1,
                    "repetition": repetition, "smoke": smoke,
                    "debt": xlfn::benchmark_support::cache_backend_debt_probe(backend),
                })
            );
        }
    }
}
