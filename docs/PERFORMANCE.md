# Performance correction record — 2026-09-26

The initial record covers the five findings reviewed against `0889a7b` and
their local corrections, based on `8dc096e`. Dated follow-ups below identify
their own measurement baselines. These local results are not a Windows/Excel
performance qualification or a release readiness decision.

## Unconditional object capabilities — 2026-09-28

Baseline: `579aa26`, the requested checkpoint of the existing working tree.
`ObjectBinding::armed` and `RawObjectLeaseGuard::armed` were initialized to
`true` by every constructor and never changed. Removing the fields makes Drop
unconditionally release the capability's one binding count or pin. Moves still
transfer the capability; failed admission constructs no guard. This adds no
pointer tags, unsafe operations, atomics, or changes to arena synchronization.

Actual production-type sizes, measured in arm64 Rust 1.98.1 libtests:

| Type | Before bytes | After bytes |
| --- | ---: | ---: |
| ObjectBinding | 40 | 32 |
| RawObjectLeaseGuard | 32 | 24 |
| BindingRecord (contains ObjectBinding) | 72 | 64 |

The layout regression test compares capabilities with their required fields.
A lifecycle test duplicates bindings and acquires two pins, moves a binding
through `Option`, and checks count transitions and exactly-once payload drop
with either the binding or the pin released last. Existing failed-admission
tests still check that neither pin counter is partially incremented.

### Allocation measurements

Saved before/after `handle_memory` executables used identical
`bench-internals async` features, the optimized bench profile, and the system
allocator on macOS arm64. They ran in before/after/after/before order; both
runs of each version reported identical allocation counts and byte deltas.
Values are requested live allocation bytes, not RSS or allocator size classes.

| Case | Before bytes | After bytes | Saved bytes |
| --- | ---: | ---: | ---: |
| Cold growth, 1,000 bindings | 1,151,460 | 1,143,460 | 8,000 |
| Cold growth, 10,000 bindings | 10,251,804 | 10,171,804 | 80,000 |
| Republish, after first 10,000 operations | 10,301,436 | 10,221,436 | 80,000 |
| Republish, after 500,000 operations | 10,301,436 | 10,221,436 | 80,000 |

The 10,000-binding growth reduction is about 0.78% of measured live bytes,
not the 20% reduction of `ObjectBinding` itself. Republish still requests
140,631 allocations for the first 10,000 operations. Empty-service bytes are
unchanged. Both versions retain 288 bytes after the first sparse fixture;
all later fixtures report zero after-drop byte delta. The benchmark exercises
binding allocation/retirement; it does not measure the memory or runtime of
generated async futures containing `RawObjectLeaseGuard`.

### Timing experiment and decision

The same saved `handle_prepare` binaries ran before-A, after-A, after-B,
before-B, after compilation and tests finished. Each case uses 50 Criterion
samples, 0.5 s warmup and a 2 s requested measurement period. Values below are
Criterion mean point estimates in microseconds per batch. Change compares the
average of the two after estimates with the average of the two before
estimates; it is not a combined confidence interval.

| Workload | Before A | After A | After B | Before B | Change |
| --- | ---: | ---: | ---: | ---: | ---: |
| cold_miss_batch_100 | 78.584 | 79.646 | 79.744 | 79.754 | +0.66% |
| warm_hit_batch_100 | 6.173 | 6.184 | 6.181 | 6.206 | -0.11% |
| distinct_key/1 | 67.615 | 68.584 | 68.643 | 67.557 | +1.52% |
| distinct_key/32 | 554.143 | 542.019 | 538.126 | 540.591 | -1.33% |
| cold_grow/10000 | 8,586.239 | 8,458.164 | 8,534.491 | 8,463.838 | -0.34% |
| republish/10_000 | 15,773.992 | 15,627.446 | 15,560.803 | 15,775.831 | -1.15% |

Keep this as a candidate for simpler capability ownership and measured memory
reduction, not as a demonstrated speedup. Single-worker distinct-key time was
1.52% worse in this experiment; the short local samples neither establish
equivalence nor qualify Windows/Excel or async-task tail latency. No extra
runtime indirection, tag masking, or synchronization was introduced to obtain
the byte reduction.

Reproduction (copy the before executables before rebuilding the candidate):

```sh
cargo bench -p xlfn --features 'bench-internals async' --bench handle_memory --bench handle_prepare --no-run --locked
path/to/handle_memory
XLFN_BENCH_MEASUREMENT_MS=2000 path/to/handle_prepare --bench 'handle_prepare/(cold_miss_batch_100|warm_hit_batch_100|distinct_key/(1|32)$|cold_grow/10000$|republish/10_000)' --warm-up-time 0.5 --noplot --save-baseline armed-before-a
```

Repeat with the respective saved executables and baseline labels
`armed-after-a`, `armed-after-b`, `armed-before-b`.

### Validation

- `xlfn` all-feature lib tests: 699 passed with process-isolated nextest and
  with serialized libtest; 10 skipped/ignored.
- All three object tests pass with strict provenance under both Stacked
  Borrows and Tree Borrows, pinned `nightly-2026-08-22`. The new `miri_` test
  is selected by the existing handles command in `just miri`.
- `xlfn` all-target/all-feature Clippy, formatting, and diff checks pass.
- x86-64 and i686 MSVC all-feature type checks pass with `blake3/pure`.
  This is supplemental cross-target checking, not native Windows/Excel evidence.

## Resident entry pointer tag — 2026-09-28

`ResidentEntry::owns_residency` used `AtomicBool::get_mut` only: it was an
exclusive ownership marker, not an atomic synchronization operation. On this
64-bit host the flag and padding enlarged each entry from 16 to 24 bytes.
The entry now stores that marker in bit zero of its node pointer, reducing
`ResidentEntry` to 16 bytes while leaving the full `u64` weight intact.

`CacheNode<V>` contains an `AtomicUsize`; a generic compile-time alignment
assertion guards the spare bit. `map_addr` preserves provenance, and
the private `node()` helper removes the tag before returning a `NodePtr`.
Restoring `NonNull` uses the constructor/alignment invariant rather than adding
a null-check branch to every lookup; masking changes only the reserved bit.
Clones clear the ownership bit. Retirement clears it before releasing the
resident pin, so repeated retirement and subsequent drop cannot release it
twice. Only exclusive access mutates the bit; the node's actual resident/pin
atomics and lookup/grace-period protocol are unchanged. `Send`/`Sync` retain
the existing `V: Send + Sync` capability bounds.

Regression coverage checks compact layout, thread bounds, zero-sized and
over-aligned payloads, untagged snapshots, clone-of-clone behavior, exactly-once
pin release, and clone/snapshot/drop after the allocation is destroyed.
`just miri-cache-resident-entry`, also called by `just miri`, runs these tests
with strict provenance under both Stacked Borrows and Tree Borrows. This is
native representation testing, not a new formal proof of pointer tagging.

### Local measurements

Both binaries use Rust 1.98.1's optimized bench profile and the same working
tree, differing only in the resident-entry representation. The baseline
executables were saved before editing. Compilation and tests finished before
the measured runs. Production `QuickCache { shards: 1 }` was selected, without
smoke mode. `cache_backends` runs three repetitions per case, with 200 ms
warmup and at least one second per repetition; throughput below is the median
in millions of operations/second. These short local runs are observations,
not statistically established speedups.

| Workload | Workers | Before | Tagged | Change |
| --- | ---: | ---: | ---: | ---: |
| Hot | 1 | 23.036 | 22.718 | -1.4% |
| Hot | 8 | 6.325 | 6.813 | +7.7% |
| Hot | 32 | 5.157 | 5.320 | +3.2% |
| Disjoint | 8 | 8.356 | 8.400 | +0.5% |
| Disjoint | 32 | 7.082 | 6.717 | -5.1% |
| Mixed | 1 | 16.605 | 16.140 | -2.8% |
| Mixed | 32 | 4.131 | 4.379 | +6.0% |
| Eviction | 1 | 1.884 | 1.862 | -1.2% |
| Eviction | 32 | 0.585 | 0.642 | +9.7% |
| LiveEviction | 1 | 1.704 | 1.686 | -1.1% |
| LiveEviction | 32 | 0.255 | 0.289 | +13.5% |
| Invalidate | 1 | 1.964 | 1.894 | -3.6% |
| Invalidate | 32 | 0.362 | 0.392 | +8.3% |
| Clear | 1 | 1.583 | 1.579 | -0.3% |

Tail latency is also mixed. Median sampled read p99 for Mixed/32 fell from
39,500 to 19,958 ns, while Hot/32 rose from 3,792 to 5,375 ns and Disjoint/32
from 4,833 to 5,833 ns. Eviction/32 writer p99 fell from 766,583 to 720,750 ns;
Invalidate/32 fell from 556,125 to 502,583 ns. Scheduling and timer granularity
affect these samples; no universal throughput or tail improvement is claimed.
All cases drained to zero pending nodes/weight. ControlledDebt retained
64 nodes / 512 bytes until reader release and drained to zero in both versions.

`cache_workloads` was run in ten alternating tagged/baseline process pairs.
The table shows medians of the repeat phase, in microseconds per trace. All
seven traces preserved hit rate and computation count in cold and repeat phases.

| Trace | Before us | Tagged us | Change | Index bytes before | Tagged |
| --- | ---: | ---: | ---: | ---: | ---: |
| fits_half_budget | 140.3 | 134.5 | -4.1% | 5,256 | 4,744 |
| fits_near_budget | 167.3 | 156.0 | -6.8% | 9,928 | 8,968 |
| interleaved_scan | 3,605.6 | 3,582.8 | -0.6% | 51,720 | 47,112 |
| changing_working_set | 1,532.2 | 1,524.2 | -0.5% | 4,104 | 3,720 |
| one_large_matrix | 4.0 | 3.8 | -5.2% | 100 | 92 |
| large_and_small_matrices | 17.5 | 16.5 | -5.9% | 2,704 | 2,440 |
| variable_weight_pressure | 19,831.1 | 19,922.6 | +0.5% | 1,536 | 1,384 |

Cold medians ranged from -3.4% (fits_near_budget) to +2.9%
(one_large_matrix); large_and_small_matrices was -0.5%. Index bytes are Quick
Cache's allocation estimate, not process RSS or payload memory. The adopted
tradeoff is a deterministic 33% entry-size reduction and roughly 8–10% lower
index storage in these traces, with improved repeat traces but some slower
single-worker/contended cases and tails. An initial checked-unmask candidate
was remeasured in reverse order after regressions; the final implementation
eliminates its redundant null-check branch. The results above describe only
the final implementation and do not establish that this branch caused every
earlier timing difference.

Reproduction (save each revision's binaries before rebuilding the other):

```sh
cargo bench -p xlfn --features 'cache bench-internals' --bench cache_backends --bench cache_workloads --no-run --locked
XLFN_CACHE_BACKENDS=quick1 path/to/cache_backends
XLFN_CACHE_BACKEND=quick1 path/to/cache_workloads
```

### Validation

Validation on macOS 27.0 arm64, Rust 1.98.1, working tree based on `8ba7679`:

- Cache tests with `cache bench-internals`: 98 passed in both process-isolated
  nextest and serialized libtest; libtest has one ignored test.
- The two new tests and the existing resident-insertion panic/creator-pin test
  passed under both Miri aliasing models (`nightly-2026-08-22`).
- Package all-target Clippy with `cache bench-internals`, formatting and diff
  checks passed.
- i686 MSVC cache-feature type check passed. x86-64 MSVC type checking passed
  with `cache blake3/pure`; the ordinary check requires unavailable `ml64.exe`.
  Native Windows execution and live Excel timing remain unverified.

## Changes and regression coverage

| Finding | Adopted correction | Regression coverage |
| --- | --- | --- |
| Refresh completion/abort reserved a notification without recording its epoch; later publishers repeatedly acquired the refresh mutex. | All notification reservations record the current publish epoch while holding the refresh mutex. A delayed publisher cannot overwrite it with an older snapshot. | A publisher must finish while the refresh mutex is held after successful delivery, failed delivery, batch drop, and uncollected-plan drop. Tests also verify the latest value, subsequent notification, and delayed-publisher epoch. |
| Every public cache endpoint access locked, hashed, and downcast the registry entry. | Memoize resolved endpoints per thread in 32 sets of two entries. Entries carry a never-reused registry identity and the full endpoint type/name key; the registry continues to own the stable cache allocations. | Warm access must finish while the registry write lock is held. Tests cover owner moves, clear, destruction/replacement, registry/type separation, and capacity overflow. The lifetime tests pass under both Miri aliasing models. |
| Derived enum output allocated a temporary owned string for every array cell. | Generate `IntoExcel::write_into` to send the static label directly to the cell sink. The generated code uses the versioned private macro facade. | Renamed and Unicode labels, sink error propagation, external macro consumers, and allocation equality with borrowed strings. |
| Disabled refinement observers still parsed/authenticated their token payloads. | Construct token payloads within the same test/refinement feature boundary as the observer. The ordinary build uses an inert payload, without authentication work. | Full-feature trace tests remain enabled; ordinary feature combinations compile. The ordinary release allocation-probe binary contains no `refinement_token` symbol. |
| Fixed ASCII handle tokens allocated call-scope UTF-16 decoding storage. | Convert the fixed-width token into a stack buffer for synchronous and pending async inputs. Non-ASCII or differently sized input still uses strict UTF-16 decoding, preserving error precedence. | Identity/authentication, malformed UTF-16/length tests, and zero-allocation checks for warmed synchronous, identity, and pending async conversions. |

## Allocation measurements

Local host: macOS arm64, Rust 1.98.1, optimized bench profile, system allocator.
Each row warms 100 operations and measures 10,000 operations. Arrays are
constructed and dropped inside measurement; handle operations use a fresh call
scope and warmed authentication. Refinement tracing is disabled.

The baseline was measured from the review snapshot at `0889a7b`; the corrected
measurements use the checked-in `value_boundary_allocations` fixture. Both use
the same 1,000-cell `Ready` labels and raw argument ingress harness.

| Operation | Allocations per operation, before | After | Requested bytes per operation, before | After |
| --- | ---: | ---: | ---: | ---: |
| 1,000 borrowed-string cells | 6 | 6 | 47,792 | 47,792 |
| 1,000 derived enum cells | 1,006 | 6 | 52,792 | 47,792 |
| Numeric input | 0 | 0 | 0 | 0 |
| Handle input | 1 | 0 | 496 | 0 |
| Handle input with identity | 1 | 0 | 496 | 0 |
| Pending async handle input | Not measured | 0 | Not measured | 0 |

There were no reallocations. Deallocation counts matched allocation counts.
These are cumulative allocation requests, not retained memory, RSS, latency, or
Excel measurements. The fixture asserts complete allocation-count equality for
enum/string output and zero allocation for warmed input conversion.

```text
cargo bench -p xlfn --features bench-internals,async --bench value_boundary_allocations --locked
```

## Public cache endpoint workload

`cache_registry` measures the public endpoint path, including lease access and
release. Setup and persistent-worker warmup are outside measurement. Worker
coordination is inside each 10,000-lookup batch. It covers 1/8/32 workers with
distinct endpoints and a single worker cycling through 1/8/16 endpoints.

The first candidate scanned an eight-entry FIFO. A short follow-up showed
regressions when cycling through eight and sixteen endpoints, so that candidate
was rejected. The adopted lookup selects one of 32 sets and checks at most two
entries. Bucket collisions or capacity pressure still fall back to ordinary
registry resolution; there is no unconditional latency improvement claim for
every endpoint distribution.

The comparison below uses the same public benchmark fixture on the review
snapshot and the correction. Values are Criterion point estimates for one
batch in milliseconds. The initial eight-entry candidate took 0.598 ms and
0.896 ms on the eight-/sixteen-endpoint cycles; the bounded two-way lookup
removed that observed regression.

| Workload | Review snapshot | Correction |
| --- | ---: | ---: |
| Distinct endpoints, 1 worker | 0.441 | 0.420 |
| Distinct endpoints, 8 workers | 9.016 | 0.965 |
| Distinct endpoints, 32 workers | 41.050 | 4.908 |
| One worker cycling 1 endpoint | 0.446 | 0.457 |
| One worker cycling 8 endpoints | 0.441 | 0.427 |
| One worker cycling 16 endpoints | 0.496 | 0.428 |

Both versions used 100 ms warmup, 300 ms measurement, and 10 samples. The
corrected eight-/sixteen-endpoint rows use a follow-up with 200 ms warmup,
2 s measurement, and 30 samples because the short eight-endpoint run had
outliers. Builds and other validation jobs were finished before these runs;
unrelated background work was not controlled. An earlier corrected run that
overlapped validation was discarded. These local sequential observations support
the contention correction but do not establish deployment-host speed ratios;
the small single-worker differences should not be treated as reliable gains.

```text
cargo bench -p xlfn --features bench-internals,cache --bench cache_registry --locked
```

Both new benchmarks are part of `just bench-ci` and `just bench-full`.

## Follow-up: typed endpoints and shared-address prefixes

Re-review found that the set selector omitted type and name length. Three
different marker types sharing one ID therefore competed for the same two
slots, as did three prefixes of one static string. The first case produced
30,000 misses in 30,000 warmed lookups despite ample unused storage.

The selector now hashes `TypeId`, registry identity, name address, and name
length with `FxHasher`. It hashes fixed-size metadata rather than scanning name
bytes, and checks the full registry/type/name key before returning a pointer.
The low bits of `finish()` select the set on both 32- and 64-bit targets.
Capacity, replacement, ownership, and the ordinary miss path are unchanged.

Two regression tests select three candidates that fit the two-way sets from a
larger pool, avoiding dependence on the bucket placement of exactly three
compiler-generated TypeIds. They then require repeated hits for distinct live
pointers. Both tests were run against the old selector first and failed:
only two candidates could fit because the omitted metadata forced every
candidate into one set. Both are included in `just miri-cache-endpoints`.

The public benchmark adds same-ID marker cycles with three/eight types and a
three-prefix cycle. Marker calls use static dispatch. One three-marker batch
has 9,999 lookups; other single-worker batches have 10,000. Setup, assertions,
and startup are outside measurement; lease access/drop and worker coordination
are inside. All nine workloads remain in `just bench-ci` and `just bench-full`.

The following comparison uses the checkout based on `4871fdb`, the identical
new fixture before/after the selector change, a dedicated release target, and
macOS arm64 / Rust 1.98.1. Values are Criterion batch point estimates in
microseconds. Warmup was 200 ms, measurement 1 s, with 20 samples. The corrected
eight-marker row uses a 2 s / 30-sample follow-up because its first run had large
outliers. No other validation commands from this task ran during timing;
unrelated background work was not controlled.

| Workload | Before | After |
| --- | ---: | ---: |
| Same ID, 3 marker types | 528.57 | 407.95 |
| Same ID, 8 marker types | 526.11 | 443.03 |
| Same address, 3 prefix lengths | 513.87 | 423.05 |
| One worker cycling 1 endpoint | 544.84 | 422.63 |
| One worker cycling 8 endpoints | 440.01 | 423.95 |
| One worker cycling 16 endpoints | 439.64 | 426.94 |
| Distinct endpoints, 1 worker | 620.85 | 441.65 |
| Distinct endpoints, 8 workers | 1,538.5 | 1,231.8 |
| Distinct endpoints, 32 workers | 4,786.3 | 4,362.2 |

The three targeted cases improved in this local comparison, and no ordinary
lookup regression was observed. The original single-endpoint runs had wide
intervals, so their apparent gains are not evidence for the selector change.
These are short local observations, not Windows/Excel speed guarantees.
Ordinary bucket collisions can still occur; fixed capacity does not promise
that every possible set of endpoint keys remains resident.

### Follow-up validation

Final workspace validation used an isolated checkout of `4871fdb` with only
this follow-up's six changed files. Their contents were verified against the
shared checkout; the results do not qualify other concurrent changes there.

- Workspace all-feature nextest: 950 passed.
- Workspace all-target/all-feature Clippy, formatting, and diff checks: passed.
- `just features`: all 28 configured feature combinations passed.
- `just miri-cache-endpoints`: all five tests passed under both Stacked Borrows
  and Tree Borrows using pinned `nightly-2026-08-22`, on the identical endpoint
  implementation in the shared checkout.
- Supplemental x86-64 and i686 MSVC-target type checks passed with
  `--all-features --features blake3/pure`. Native Windows builds and live Excel
  execution remain unverified.

## Initial correction validation and limits

- Workspace all-feature nextest: 940 passed, 10 skipped.
- Workspace all-target/all-feature Clippy: passed.
- `just features`: all 28 configured feature combinations passed.
- `just panic-boundaries`: passed, with 49 reviewed boundaries.
- `just miri-cache-endpoints`: all three lifetime/equal-name tests passed under
  both Stacked Borrows and Tree Borrows using the repository's pinned
  `nightly-2026-08-22`.
- Release allocation assertions: passed.
- All six public registry benchmark workloads completed in a short local run.
- Supplemental MSVC-target type checks for x86-64 and i686 passed with
  `--all-features --features blake3/pure`.

The ordinary x86-64 MSVC-target check could not build BLAKE3's assembly because
this Mac lacks `ml64.exe`. A supplemental check with the dependency's `pure`
feature enables Rust type checking without that assembly toolchain; it does not
qualify the ordinary Windows build. Native Windows CI, live Excel/COM behavior,
and controlled deployment-host timing remain unverified by this local work.
