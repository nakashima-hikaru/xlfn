# Quick Cache + xlfn shared flight: phase two

**Historical gate verdict: retain Moka.** The bounded flight layer is feasible and substantial
throughput gains were measured, but mixed32 tails and retirement debt fail the
fixed gates. Code size is not the rejection reason. This conclusion applies to
the tested configuration; it does not rule out other Quick Cache designs.
The subsequent [production decision](resident-index-qualification.md) explicitly
accepts these tradeoffs and adopts this Quick Cache configuration. The gates
below remain historical selection criteria, not production test thresholds.

The expanded boundary permits a separate per-key flight registry owned by
`CalculationCache`. Flights retain only completion/error state, never NodePtr,
and disappear from the registry on success, error, or panic. Waiters relookup
after success; errors are shared and panic permits retry. Initializers and
native cache insertion run outside flight locks. Node pins, residency,
admission, generation, retirement/reclamation and callback ordering stay fixed.

## Predeclared gates (2026-09-07, before formal timings)

One candidate: Quick Cache 0.7.0 plus a single miss-only flight registry mutex.
Quick Cache uses one native shard to preserve the exact weight budget: its
default per-shard rounding exceeded the budget in phase one. There is no custom
resident capacity/policy layer and no shard-count tuning in this experiment.

- Geometric mean speedup over the same six warm/hot/disjoint Criterion cases
  must be at least 1.10; no individual read time may exceed 1.10 times Moka.
- Mixed32 throughput must be at least 1.10 times Moka; eviction32 and
  invalidate32 throughput must be at least 1.00 times Moka.
- No median p99 regression in any populated supplemental hit/writer latency
  stream, nor mixed32 hit p50/p95 regression. No additional tolerance.
- Previous guardrails remain: mixed32 hit rate at least 99% of Moka, other
  mutation and Criterion reclamation throughput at least 90% of Moka,
  matching peak pending nodes/weight no higher than Moka, and full final drain.
- Both Stacked and Tree Borrows must pass with leak/alias checking enabled.

The prior workload definitions, 200 ms warmup, 1 s measurement windows,
20 Criterion samples, three repetitions, and separate supplemental latency
batches are retained. Backend order alternates per repetition. Comparisons use
medians across repetitions; retirement peaks compare repetition maxima.

Memory separates common resident/index lower bounds, resident node estimates,
retained lease payload, queued debt, and the new flight registry. Quick Cache's
native memory statistics are reported separately from the common lower bound.
Flight registry capacity may remain allocated after all flight objects leave.

## Results

Moka 0.12.16 baseline; Apple M1, 8 logical CPUs, 16 GiB RAM, aarch64 macOS;
Rust 1.98.0. The 32-thread
cases oversubscribe this host. Three short local repetitions support this
configuration's decision, not a universal ranking. Timings and latency probes
are separate; their throughputs and tails must not be treated as one sample.

| Gate | Quick/Moka result | Outcome |
| --- | ---: | --- |
| Six-read geometric mean speedup | 1.757 | Pass |
| Mixed32 throughput | 1.342 (4.903 vs 3.654 Mops/s) | Pass |
| Eviction32 throughput | 1.185 | Pass |
| Invalidate32 throughput | 2.677 | Pass |
| Mixed32 hit p99 time | 3.765 (26.042 vs 6.917 µs) | Fail |
| Mixed32 writer p99 time | 1.034 | Fail |
| Invalidate32 peak queued nodes / weight | 81 / 648 vs 65 / 520 | Fail |
| Criterion churn32 throughput, 64 B / 64 KiB | 0.882 / 0.842 | Fail (previous 0.90 floor) |

The six read speedups are warm 4.407, hot1 4.397, hot8 1.075, hot32 1.293,
disjoint8 1.020, and disjoint32 1.072. The separate supplemental disjoint8/32
throughput ratios are 0.851/0.784, so read improvements are not uniform across
harnesses. No candidate/configuration was tuned after observing these results.

Mixed32 hit p99 repetitions were 6.917/7.000/6.834 µs for Moka and
60.667/26.042/23.417 µs for Quick: every candidate repetition is worse than
every baseline repetition. Hit p50/p95 ratios are 1.168/1.804. Both hit rates
were 100%. Invalidate32 queued-node peaks were 65/65/65 versus 77/77/81;
weights are eight times these counts. All other matching peak-debt comparisons
passed. All supplemental final drains were zero; controlled scoped clear queued
64 nodes / 512 weighted bytes and drained to zero for both backends. Separate
untimed shutdown checks verified final zero debt for all 12 Criterion
reclamation cases per backend, after workers and retained leases exited.

At the mixed32 snapshot both backends retained 64 entries, weight 512, with
3,072 estimated resident-node bytes and a 2,048-byte common index lower bound.
Quick additionally reported native entry/map storage of 15,360/5,128 bytes;
these are not comparable to Moka's opaque index overhead. Its empty flight
registry retained 28 slots (448-byte slot lower bound), with zero active flights.
Both live-eviction32 probes retained 8,192 payload bytes, potentially overlapping
resident storage. Estimates exclude allocator rounding, Arc allocation headers
and heap storage in keys; they are not RSS or total-memory comparisons.

## Correctness and cleanup

The flight core is 104 lines excluding tests and the 11-line memory helper.
It uses no unsafe code. Native tests of shared-error identity, three waiting full-cache
callers, panic/retry, immediate flight removal, independent-key progress,
oversized destruction and exact-capacity checks passed. The existing 11 common
protocol tests plus four added tests passed under both Stacked and Tree Borrows.
The same historical Miri concurrency bounds were used for the common tests.
Leak and alias checking remained enabled. Nightly 1.100.0 (c656540d6) emitted the
existing integer-to-pointer warning in `parking_lot_core` 0.9.12,
`word_lock.rs:320`; these passes do not erase that diagnostic.

Two unchanged Moka-policy tests expected a full-budget item to remain resident;
Quick rejected those items, so those assertions failed. They were not weakened
or represented as passing. The common lifetime and rejection tests passed.
Windows i686/x86_64 MSVC library Clippy checks passed with `blake3/pure`; Windows
runtime behavior was not tested. Quick's MSRV 1.85 fits the repository's 1.98.
Its inspected normal dependency branch has no `crossbeam-epoch`; retaining
Moka at this stage left the cache's existing epoch dependency unchanged.

The experiment implementation and frozen gates are preserved at `a2dfd27`.
Measurement evidence comprises 120 Criterion timings, 72 reclamation probes,
and 90 supplemental observations. Evidence is retained at `b513582` in Git history only.
The temporary flight layer, Quick adapter/dependency, selection feature,
qualification tests, scripts, harness and raw files were removed from the final
tree at `d5c864f`. At that point production code and regression bounds matched
`fe9a921`; the later adoption restores the candidate as the sole production backend.
