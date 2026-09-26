# Performance correction record — 2026-09-26

This record covers the five findings reviewed against `0889a7b` and their local
corrections. Validation uses the working tree based on `8dc096e`, retaining its
collection conversion and lifecycle changes. It is not a Windows/Excel
performance qualification or a release readiness decision.

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
