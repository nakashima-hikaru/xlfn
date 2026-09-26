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

## Validation and limits

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
