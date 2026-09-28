# Production adoption: shared RTD publisher and prepared formula inputs

Date: 2026-09-29. Base: `3608a70` (cache flight errors changed from Arc to Box).
This record supersedes the adoption holds in PERFORMANCE_REDESIGN_EXPERIMENTS.md;
those earlier measurements remain historical prototype evidence.

## RTD publication

`RuntimeServices` owns a lazily started publisher pool, currently one worker per
runtime generation. All `RtdChannelSource` subscriptions in the generation share
it, including subscriptions from different registered sources. Producers remain
per subscription: N subscriptions use N+1 workers. Public channel construction
and queue-full behavior are unchanged. There is no latest-value coalescing before
PublishCore: accepted values retain per-topic FIFO and publication sequencing.

Each topic has at most one ready-queue entry. The worker publishes up to 32 values
then appends a still-busy topic to the tail. This bounds consecutive work for one
topic, provided publication/notification returns; a blocked notifier can delay
all topics on this worker. No claim of bounded wall-clock latency is made.

The subscription owns its publication mutex and sink revocation barrier. Cancel
closes sender admission and discards queued values. Disconnect waits for the
current publication batch, removes the sink under the same mutex, removes its
scheduler reference, and joins the producer before reporting either error.
Pending queue entries may retain closed storage but cannot use a revoked sink.
A contained topic panic records `XllError::Panic`, closes that channel and leaves
the shared worker available to other topics. Arbitrary panic payload destruction
uses the existing framework containment policy.

Pool startup is fallible and happens before installing the sink. Producer-start
failure drops the subscription barrier. Generation close disconnects every
subscription before stopping and joining the pool; RuntimeServices Drop also
joins as a final ownership backstop. Senders cannot retain worker ownership.

## Two-phase synchronous UDF ingress

Generated synchronous wrappers now prepare every argument, finish semantic
identity, and enter `ExcelReturn::invoke`. Materialization happens inside its
initializer closure. A warm formula revision does not invoke that closure.
Plain-return wrappers use the same pipeline with already-owned prepared values.
Async wrappers fully convert inputs before launching their task; no borrowed
XLOPER12 data crosses the async boundary.

`PreparedArgument` distinguishes an owned value from a call-borrowed validated
input and its materializer. Formula-mode built-in scalar/String inputs and
`Matrix<T>` with preflight-capable elements can defer conversion. Matrix shape,
source/output budgets, every cell's type/value validity, and the complete
semantic identity are checked before lookup. String identity streams UTF-16 as
length-prefixed UTF-8, matching owned String identity without per-cell allocation.
Materialization allocates the final vector with exact capacity.

`FromExcel::PREFLIGHT` explicitly opts a custom converter into this contract;
`preflight_identity` must preserve conversion errors and semantic identity, and
conversion must not have observable side effects. The default is eager
conversion, retaining the converted value exactly once. Context-bearing Handle
parameters always resolve/type-check/pin before lookup. Default expressions are
evaluated once before lookup, hashed, and retained as owned prepared values.
Optional, borrowed, and other collection parameter implementations currently
retain their eager conversion policy; they use the same prepared-value pipeline.
There is no public compatibility alias or alternate generated wrapper.

The benchmark now calls production `ArgumentContext::prepare`, not the previous
standalone prototype. It includes real formula lookup/publication but not Excel
or COM. Cold paths deliberately pay a second traversal; adoption prioritizes
avoiding allocation on warm calls, not universal latency improvement.

## Validation and measurements

See the appended results below. Native macOS tests, cross compilation, and Miri
do not establish Windows/Excel runtime performance. No new native atomic or
formal refinement theorem is claimed for the publisher scheduler.

### Production ingress measurement

Rust 1.98.1, macOS arm64, optimized `bench-internals cache rtd async` build,
without refinement. Two processes each use ABBA order (four means per path).
Other tests/builds were idle while timing. Allocation probes are separate from
timing. Values below are microseconds/call; these include ingress, identity,
revision lookup/publication, and token construction, not generated ABI or COM.

| Input | Path | Eager us | Prepared us | Change |
|---|---|---:|---:|---:|
| f64 | warm | 0.357 | 0.357 | +0.0% |
| f64 | cold | 1.049 | 1.052 | +0.3% |
| f64 | changed_last | 1.044 | 1.043 | -0.1% |
| matrix_f64_1k | warm | 14.783 | 14.755 | -0.2% |
| matrix_f64_1k | cold | 15.425 | 17.752 | +15.1% |
| matrix_f64_1k | changed_last | 15.432 | 17.769 | +15.1% |
| matrix_f64_100k | warm | 1162.733 | 1150.377 | -1.1% |
| matrix_f64_100k | cold | 1162.471 | 1379.168 | +18.6% |
| matrix_f64_100k | changed_last | 1163.849 | 1382.358 | +18.8% |
| matrix_string_10k | warm | 736.655 | 564.078 | -23.4% |
| matrix_string_10k | cold | 738.970 | 906.165 | +22.6% |
| matrix_string_10k | changed_last | 740.276 | 905.453 | +22.3% |

Warm numeric 100k: allocations 3 -> 2, requested bytes 806,106 -> 6,106.
Warm Unicode String 10k: allocations 10,003 -> 2, bytes 376,106 -> 6,106.
Numeric cold latency regresses more than the specialized prototype because the
generic production materializer retains checked per-cell conversion. The adopted
tradeoff is reduced warm allocation and string latency, with about 19% numeric
and 23% string cold overhead in these fixtures. Do not describe this as a
universal improvement or reuse the prototype's approximately 3% cold result.

Raw data: [production A](measurements/2026-09-29-redesign/production-two-phase-a.jsonl),
[production B](measurements/2026-09-29-redesign/production-two-phase-b.jsonl).

### Production RTD measurement

Each process runs one warm-up and six measured rounds of 1,000 values/topic.
Two processes per subscription count; final value and sequence assertions pass.
The old column is the earlier same-machine experiment, not a fresh interleaved
baseline. Scheduler variance is substantial at 512 topics; no significance
claim is made. Setup/teardown are excluded from update-round timing. The new
setup field includes lazy production pool startup.

| Subscriptions | Earlier per-topic median mean, ms | Generation pool median mean, ms | New process medians, ms | Workers before -> after |
|---:|---:|---:|---|---|
| 32 | 6.595 | 4.008 | 4.069, 3.947 | 64 -> 33 |
| 128 | 30.022 | 14.440 | 14.481, 14.400 | 256 -> 129 |
| 512 | 156.816 | 47.868 | 62.572, 33.163 | 1024 -> 513 |

Raw data: [production RTD](measurements/2026-09-29-redesign/production-rtd-topology.jsonl).
Reproduce with `protocol_costs --topology`, `XLFN_TOPOLOGY_SUBSCRIPTIONS` set to
32/128/512 and `XLFN_TOPOLOGY_PUBLISHERS=0` (production selector).

### Verification

- Workspace all-feature nextest: 977 passed, 11 skipped.
- Serialized xlfn all-feature libtests: 709 passed, 10 ignored.
- Workspace/all-target/all-feature clippy, format, diff whitespace, and panic
  boundary policy checks pass. Generated wrapper compile-pass/fail tests pass.
- Core-only check and i686/x86_64 Windows MSVC all-feature checks with
  `blake3/pure` pass. This is cross compilation, not Windows execution.
- RTD channel Miri: five cancellation/drain/lifetime/panic cases pass with both
  Stacked Borrows and Tree Borrows. Strict provenance is unsupported by the
  contended parking_lot 0.9.12 word-lock integer casts, so RTD uses the repository's
  ordinary Miri mode. Native timeout assertions use an untimed Condvar wait in
  Miri because the dependency's timed wait calls isolated `gettimeofday`.
- Production two-phase benchmark fixtures: two cases pass under strict
  provenance with each of Stacked Borrows and Tree Borrows. They cover same
  identity/token, no warm materialization, changed-last-cell, and invalid final
  numeric/UTF-16 cells rejected before factory execution.
- Topic FIFO/batch cancellation, publication barriers, producer panic/error,
  setup unwind, shared-worker panic isolation, round-robin requeue ordering,
  terminal generation-pool stop, and eager custom-converter exactly-once behavior
  have native regression coverage.

No live Windows Excel/COM run was performed. These production changes remain
uncommitted; only the separately requested cache flight Arc-to-Box change is
committed as `3608a70`.
