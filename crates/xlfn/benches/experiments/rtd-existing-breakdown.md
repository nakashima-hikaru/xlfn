# RTD existing-topic residual breakdown

The additional cost of ten-part topics is dominated by destruction of the
consumed input topic. In this local diagnostic, the production existing-path
residual is **38.77 µs per 256 subscriptions**, close to the previous 40.09 µs.
A two-factor decomposition estimates **34.03 µs for destruction/deallocation**
and **6.15 µs for full topic comparison**. The remaining control and calibration
terms total -1.42 µs. Commit-only time changes by just 0.03 µs.

The stable-slot arena implementation was not changed. Diagnostic methods and
exports are gated by `bench-internals` plus `rtd`; the ordinary `prepare` method
is unchanged. The default checkout still uses its preceding non-owning map.
The supplemental `rtd-existing-breakdown.patch` applies after the slot-arena
candidate patch and registers the two experiment sources beside this report.

## Controls and timing

Each fixture seeds 256 committed pending subscriptions with the existing topic
construction. It validates singleton identity buckets and exact topic equality
before timing, and stores IDs for the commit-only probe. Every case receives
the same seeded fixture; commit-only additionally seeds one extra reservation
per ID. Inputs, validation, seeding, and final fixture teardown are excluded.

- `production`: ordinary owning `prepare`, then reservation commit.
- `owned_compare`: borrowed existing-only control with full comparison, followed
  by explicit input destruction and then commit. Destruction happens after the
  prepare lock/gate is released and before commit takes the lock again.
- `borrowed_compare`: the same comparison and reservation path, retaining all
  input topics until fixture teardown outside timing.
- `owned_hash_only` / `borrowed_hash_only`: corresponding ownership controls
  that read the singleton bucket's arena record without full topic comparison.
  The fixture prevalidates that assumption. These controls deliberately do not
  provide general collision-correct identity and are never used by production.
- `drop_only`: destruction of the same input topics, with the canonical catalog
  still alive, without lookup or reservation operations.
- `commit_only`: finish the extra preseeded reservations using their logical IDs.
  Includes the commit catalog lock, ID lookup, arena access, and phase update;
  excludes the initial prepare and its identity lookup. It is a separate probe,
  not a term added to the full-path differences below.

Neither hash-only control rehashes topic strings. Both use the topic's existing
cached hash and still perform the identity index and generation-checked arena
lookup. Borrowed controls keep their input storage alive, so cache/allocator
interactions need not be additive with owned controls.

## Results

Apple M1, macOS 26.6.2, Rust 1.98.0, release profile, System allocator. Three
sequential runs used 0.5 s warm-up, 3 s measurement, and 30 samples per case.
Parts 1 and 10 were adjacent within each case; case order was fixed across runs.
Numbers below are medians of the three Criterion slope point estimates, in
microseconds per batch. The allocation counter was in a different executable
and added no tracking overhead to these timings.

| Case | 1 part | 10 parts | Additional nine parts |
| --- | ---: | ---: | ---: |
| `production` | 13.72 | 52.48 | +38.77 |
| `owned_compare` | 14.01 | 53.46 | +39.46 |
| `borrowed_compare` | 10.01 | 15.00 | +4.99 |
| `owned_hash_only` | 14.10 | 46.97 | +32.87 |
| `borrowed_hash_only` | 9.03 | 8.31 | -0.72 |
| `drop_only` | 6.46 | 39.27 | +32.80 |
| `commit_only` | 2.65 | 2.68 | +0.03 |

## Decomposing the additional nine parts

Let Δ(case) be its 10-part minus 1-part time, using the table medians. The two
ways to measure each factor bound its dependence on the other factor:

| Factor | With the other factor | Without the other factor | Average |
| --- | ---: | ---: | ---: |
| Input destruction/deallocation | 34.47 | 33.59 | **34.03** |
| Full topic comparison | 6.59 | 5.71 | **6.15** |

Specifically:

```text
destruction, with comparison    = Δ(owned_compare) - Δ(borrowed_compare)
destruction, without comparison = Δ(owned_hash_only) - Δ(borrowed_hash_only)
comparison, with destruction    = Δ(owned_compare) - Δ(owned_hash_only)
comparison, without destruction = Δ(borrowed_compare) - Δ(borrowed_hash_only)
```

Averaging the two contexts allocates their 0.88 µs interaction equally:

```text
controlled residual = 34.03 + 6.15 - 0.72 = 39.46 µs
production residual = 39.46 - 0.69        = 38.77 µs
```

The -0.72 µs is the measured parts delta of the borrowed hash-only core. It is
not a claim of negative work: layout/cache effects and measurement variation
can make a control slightly faster for one fixture shape. The -0.69 µs is the
calibration difference between the ordinary path and the existing-only control.
These are algebraic contrasts, not independently additive CPU-stack samples or
statistical confidence intervals for the final attribution. Per-run estimates
and intervals are preserved in `rtd-existing-breakdown-results.json`; the first
run was slower in several controls, so exact per-stage nanoseconds should not
be inferred from these short measurements.

The independent `drop_only` residual is **32.80 µs**, corroborating that most
of the added latency comes from topic teardown. Commit-only residual is
**0.03 µs**; index, arena access, gate, and reservation processing without topic
comparison/destruction show no positive part-count scaling in this experiment.

## Allocation evidence

A separate executable wraps System solely to count operations. It does not
report latency. Setup and fixture teardown run with counting disabled.

| Ordinary existing path | 1 part | 10 parts | Difference |
| --- | ---: | ---: | ---: |
| Allocations | 0 | 0 | 0 |
| Reallocations | 0 | 0 | 0 |
| Deallocations | 512 | 2,816 | **2,304** |
| Freed requested bytes | 13,312 | 133,120 | 119,808 |

Each input owns one boxed string-header array plus one heap payload per part:
256 × (parts + 1) deallocations. The additional 2,304 frees are exactly
256 × 9 extra string payloads. The header array is freed once in both cases,
with a larger layout for ten parts. Freed requested bytes include string
capacity and array storage; they are not logical text bytes or an RSS delta.
Borrowed and commit-only controls reported zero allocation/deallocation calls
inside measurement. Owned controls and drop-only matched the production counts.

## Implication

Another arena layout change would not directly target the dominant residual.
The next useful experiment would avoid constructing/consuming an owned topic
for an existing lookup, or accept a borrowed topic representation and materialize
ownership only on a miss. That is an API/ownership experiment, not an implemented
change here; construction would also need an end-to-end benchmark because this
fixture excludes it. Full equality must remain collision-correct. Eliminating
string comparison alone would leave most of the observed residual intact.

## Reproduce and validation

Start with the non-owning map checkout, then apply the arena candidate followed
by the supplemental diagnostic patch; the `.rs` experiment sources are already
present in this checkout:

```sh
git apply crates/xlfn/benches/experiments/rtd-slot-arena.patch
git apply crates/xlfn/benches/experiments/rtd-existing-breakdown.patch
CARGO_TARGET_DIR=/absolute/path/to/diagnostic-build cargo clippy \
  -p xlfn --benches --all-features --locked
XLFN_BENCH_MEASUREMENT_MS=3000 CARGO_TARGET_DIR=/absolute/path/to/diagnostic-build \
  cargo bench -p xlfn --bench rtd_existing_breakdown \
  --features "bench-internals rtd" --locked -- \
  --warm-up-time 0.5 --sample-size 30 --save-baseline run-1
CARGO_TARGET_DIR=/absolute/path/to/diagnostic-build cargo bench \
  -p xlfn --bench rtd_existing_allocations --features "bench-internals rtd" --locked
```

Repeat timing with baseline names `run-2` and `run-3`, without concurrent builds
or tests. A focused test exercises all 14 case/part combinations and verifies
seeded catalog invariants; it passed. Benchmark Clippy and both release builds
passed. The catalog/arena source hash is unchanged from the prior candidate.
Full local outputs are in `target/rtd-existing-breakdown`. These results are
local diagnostics, not Windows or real-Excel qualification.
