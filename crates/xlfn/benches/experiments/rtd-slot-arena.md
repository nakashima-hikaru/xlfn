# RTD stable slot arena candidate

Decision: retain this as an experimental candidate. The 1-part existing path
meets the 18 µs goal and new/churn improve, but 10-part existing has a 55.57 µs
median, above the strict 54 µs target. The complete acceptance set is not met.

This experiment replaces the non-owning catalog's entry HashMap with reusable,
generation-checked slots. The candidate is kept in `rtd-slot-arena.patch`; the
default runtime in this checkout remains the non-owning map implementation.
The patch applies on top of that implementation, including its `rtd_prepare`
benchmark and collision tests, rather than directly on revision `a65833b`.

## Implementation

- `SubscriptionSlot` is a pair of `u32` index and generation fields. A free list
  reuses slots without moving surviving records. Generation overflow aborts
  instead of wrapping; clear preserves and advances generations.
- Arena records own both their logical `SubscriptionId` and `SubscriptionEntry`.
  A separate `slot_by_id` map serves lifecycle operations and verifies the record ID.
- Identity buckets contain slots. `prepare(existing)` compares the canonical
  topic in the arena and updates reservations through the same slot. It does not
  consult `slot_by_id` in that lookup/update path. The returned reservation still
  uses its logical ID for later commit/rollback, as before.
- Admission plans an available slot while holding the catalog mutex, without
  consuming it. Pending capacity, bytes, logical-ID overflow, slot-index range,
  and source-ref admission all succeed before the arena, ID map, or counters
  change. Recoverable admission errors therefore need no slot rollback.
- Removal erases the identity bucket member and ID mapping before removing the
  arena record and advancing its generation. Topics remain uniquely owned; no
  shared ownership or retained topic copy is introduced.

The arena retains storage up to its slot high-water mark. Pending-entry scans
and server-termination iteration visit this storage, including holes; the
candidate does not add a pending-count cache or compact live entries.

## Correctness checks

The candidate passed `just quick`: formatting, the panic-boundary audit,
workspace Clippy, and 794 nextest tests; seven tests were skipped by the normal
profile. A separate same-process RTD run passed 118 tests. The added cases cover
stale read/mutation/removal tokens after reuse, non-reused logical IDs, free-list
high-water behavior under collisions, admission failure without partial state,
clear invalidation, stale identity candidates, and fail-stop generation overflow.

Catalog assertions reconcile every occupied arena record, logical-ID mapping,
identity bucket, source reference count, and pending-topic byte count. Collision
buckets reject duplicate full topics and duplicate indexed records.

The initial shared build directory caused compile-fail snapshot differences in
absolute source paths. Re-running with a candidate-specific build directory
passed without changing any expected diagnostics. Both benchmark executables
were also built separately and copied before timed runs; their hashes differ.

## Reproduce

Use two checkouts containing the preceding non-owning map change and the same
`rtd_prepare` fixture. Apply the candidate patch only to the second checkout:

```sh
git apply --check /path/to/rtd-slot-arena.patch
git apply /path/to/rtd-slot-arena.patch
CARGO_TARGET_DIR=/absolute/path/to/candidate-build just quick
```

Give each checkout its own build directory. Run the same six benchmark cases
with 256 subscriptions per batch, one or ten parts, the System allocator, and
release optimization. Input construction, existing-topic seeding, and runtime
teardown stay outside the measured section; prepare/commit or prepare/rollback
are included. No Excel/COM connections are made.

```sh
XLFN_BENCH_MEASUREMENT_MS=5000 CARGO_TARGET_DIR=/absolute/path/to/build \
  cargo bench -p xlfn --bench rtd_prepare --features "bench-internals rtd" \
  --locked -- --warm-up-time 1 --sample-size 50 --save-baseline map-1
```

Use distinct baseline names for each variant and repetition. This experiment
runs three pairs in the order map/slots, slots/map, map/slots after all builds
and tests have finished. Results below use the median of the three Criterion
slope point estimates for each case. Ratios compare these medians; they are not
a paired statistical confidence interval or a production performance guarantee.

## 2026-09-09 result

Host: Apple M1, macOS 26.6.2, `aarch64-apple-darwin`, Rust 1.98.0
(`88d9e12ae`, 2026-08-18). Times are microseconds per 256-subscription batch.
The baseline is the current non-owning map, remeasured in this comparison;
it is not the older owning-index measurement.

| Operation | Parts | Map median | Slots median | Time change | Criterion | Result |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| new | 1 | 56.06 | 34.09 | -39.2% | ≤ +5% | Pass |
| existing | 1 | 20.87 | 15.48 | -25.8% | ≤ 18 µs | Pass |
| churn | 1 | 68.46 | 52.75 | -23.0% | ≤ +5% | Pass |
| new | 10 | 58.52 | 34.77 | -40.6% | ≤ +5% | Pass |
| existing | 10 | 60.86 | 55.57 | -8.7% | ≤ 54 µs | Not met |
| churn | 10 | 103.69 | 87.72 | -15.4% | ≤ +5% | Pass |

The candidate existing/1 estimates were 15.48, 15.26, and 15.62 µs across the
three runs. Existing/10 was 54.37, 55.81, and 55.57 µs: all were above a strict
54 µs cutoff, though close to the requested approximate target. Its median is
2.9% over that cutoff. New/churn stayed comfortably within the +5% allowance.

The results support removing entry-map lookup costs on the short-topic reuse
path. They do not isolate the remaining cost for ten-part topics, and do not
justify introducing shared topic ownership. A next experiment could separate
full topic comparison from destruction of the consumed input topic; neither
was changed here. Existing reservation commit continues to resolve the logical
ID, so these timings are broader than the identity lookup alone.

No additional retained topic copy is present in the candidate's ownership
structure. Collision behavior passed the regression tests. Allocator traffic,
RSS, high-water iteration cost after a much larger historical peak, Windows,
and real Excel execution were not measured in this local comparison.

Per-run estimates and 95% intervals, median summaries, source/fixture hashes,
and executable hashes are recorded in `rtd-slot-arena-results.json`. Full local
Criterion output is under `target/rtd-slot-qualification`; the expanded
candidate checkout is `target/rtd-slot-candidate`. The patch is the durable
candidate artifact, and applies cleanly to the current non-owning tree.

The [existing-topic residual breakdown](rtd-existing-breakdown.md) follows up
on the ten-part cost while keeping this arena implementation unchanged.
