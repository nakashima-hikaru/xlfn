# Work-elimination experiments — 2026-09-29

> Production adoption follows in [PERFORMANCE_REDESIGN_PRODUCTION.md](PERFORMANCE_REDESIGN_PRODUCTION.md). The decisions and numbers below describe the earlier experiment.

This record evaluates the proposed ingress, RTD publisher, cache read-scope,
and admission redesigns against `cc845cd` plus the pending Flight error
`Arc` -> `Box` change. Handle/Object SSOT and bounded metadata changes are
already present. Backward compatibility is not an acceptance requirement.

## Scope and reproducibility

The new `two_phase_ingress` executable exercises owned raw XLOPER12 fixtures,
normal call admission, semantic identity, and real FormulaHandleService
`prepare_observed`/token publication. The candidate is gated by
`bench-internals`; it does not change generated wrappers or the public input
contract. There is no compatibility fallback or runtime type guessing.

Both paths use the same raw input and formula service. The eager path uses
production ArgumentContext conversion. The prepared path validates numeric
and string grids, records the same semantic fingerprint, and retains a
call-borrowed grid until the initializing caller needs an owned value. A scalar
retains its converted f64. Warm hits never materialize. Numeric materialization
uses the validated grid without repeating budget, finite-value, or cell
validation; String materialization currently uses the ordinary converter.

The UTF-16 candidate streams the same length-prefixed UTF-8 encoding as owned
String, including supplementary characters. It does not hash raw UTF-16 for
semantic String inputs. Source/output byte limits, matrix shape, nested-array
rejection, integer conversion and non-finite rejection remain in preparation.
Fingerprint hashing still allocates its large-argument buffer. This is not an
allocation-free or O(1) warm path.

Measurements use optimized Rust 1.98.1 on the local macOS arm64 host with
`bench-internals cache rtd async`, **without refinement**. No builds or tests
ran concurrently with the measured executables. Two processes each run eager,
prepared, prepared, eager for every case. Iterations per sample are 100,000
for scalar, 10,000 for 1k numeric, 200 for 100k numeric, and 300 for 10k string.
Reported times are means across four process-local sample averages per path.
They are exploratory point estimates, not confidence intervals or Windows data.

The cold fixture removes prior topics outside timing. The changed-last fixture
seeds the prior input and changes only the last cell outside timing; the timed
call includes identity mismatch and publication. These cases isolate ingress
and publication, not a realistic costly user factory. Per-call timers and a
disabled allocation-counter branch remain in both timing paths. Allocation
counts/bytes are a separate single call after timing; bytes are cumulative
requested allocation bytes, not peak live memory or RSS.

```sh
cargo bench -p xlfn --features 'bench-internals cache rtd async' --bench two_phase_ingress
cargo bench -p xlfn --features 'bench-internals cache rtd async' --bench protocol_costs --no-run
# Run the emitted protocol_costs executable with --topology for each setting:
# XLFN_TOPOLOGY_SUBSCRIPTIONS=32,128,512; XLFN_TOPOLOGY_PUBLISHERS=0,1,4.
# Run 0,1,4,4,1,0 publishers per subscription count. Zero is production.
```

## Decisions and public-contract direction

### Formula inputs

A production design must introduce a single explicit prepare/materialize
contract with an associated prepared representation, not an identity-only
trait alongside an independent conversion implementation. Custom types must
implement the new contract explicitly; an owned prepared value is legitimate
when their conversion cannot be deferred. No blanket legacy conversion
fallback is planned. Built-in scalar preparation retains its converted value;
borrowed and Handle inputs must retain their call lifetime and validated
capabilities. Resolve/type-check Handle inputs before lookup, rather than
fingerprinting unvalidated token bytes.

All input errors must precede a warm return. Default expressions must be
executed once and their exact resulting value retained. Only the single-flight
initializer should materialize; followers should drop preparation without
running user conversion again. Generated synchronous wrappers currently convert
before `ExcelReturn::invoke`; moving that boundary is required. Raw borrowed
preparations must never escape into async work. Production promotion also needs
custom/default/Handle and concurrent-follower tests, plus macro/UI feature
matrix coverage. The prototype deliberately does not claim those are done.

### RTD

The measured pool is the existing benchmark-only shared-publisher topology.
Production already dequeues batches of up to 32 values. Both topologies retain
one producer thread per subscription: total workers change from 2N to N+P,
not N to P. The probe sends 1,000 updates per topic, validates final values and
sequence deltas, and includes bounded-queue overload retries. Pool setup is
excluded from the reported subscription setup time. It does not time Excel/COM.

The preferred ownership design for a production pool is a generation/service
owner for workers, with subscription-owned cancellation and in-flight publication
barriers. Disconnect must withdraw scheduling, dispose queued values, wait for
that topic's active publication capability, then release its sink and producer.
Pool close must stop admission and join workers after subscriptions drain.

The existing experimental implementation is **not ready for direct promotion**:
`publishing = true` is manually cleared at the end of `publish_batch`. A worker
panic before that tail can strand a disconnect waiter. Startup/thread-spawn
failure, panic containment, per-topic fairness and skewed workloads need explicit
coverage before choosing a production default. The current benchmark uses
uniform i32 topics and cannot establish any of these failure guarantees.

Latest-value coalescing is a separate API decision, not a pool optimization.
It changes queue-full errors, accepted-update sequence meaning, drop timing,
and which publication failures are observed. No silent coalescing or legacy
queue shim is introduced by this work.

### Cache read scopes

Keep ReadScope benchmark-only. It holds a reader permit across caller code;
`clear` waits for readers under its serialization lock. Same-thread clear,
destructor reentry, and cross-thread clear followed by a reader's attempt to
acquire that lock can cause circular waits. Generic Hash/Eq may also call user
code. A closure API alone does not prevent this.

Promotion would require an explicit invalidation/drain contract and an audited
nonblocking/reentrancy policy covering both value and key retirement domains.
A blanket `current_thread_may_be_reading` check inside the final drain is not
sufficient if the caller already waits for a lock held by a draining thread.
Do not add that partial fix or a public scope API based only on historic timings.

### Admission

Retain separate module-ingress and runtime-generation authorities. Ingress
counts opening/closing rejection paths through return; generation admission
protects runtime state. One successful generation snapshot does not certify
that rejected exports have left the DLL. Any future composition must preserve
both obligations and native unload ordering. Normal builds' CallObservation is
PhantomData; refinement builds add tracing and must not be used to estimate
production fixed cost. The benchmark compares ingress, full admission, and
scalar-return/free batches; their difference is not proof that all of the extra
work can be removed.

## Measurements and validation

Results and final validation are appended below. Raw measurements are retained
in `docs/measurements/2026-09-29-redesign/`. No benchmark in this record establishes
native Windows or live Excel correctness/performance, nor a new formal proof.

### Two-phase results

Times are microseconds per call. Allocation rows include the returned token and
fingerprint buffer, not just argument output.

| Input | Case | Eager us | Prepared us | Change | Eager allocations / bytes | Prepared allocations / bytes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| f64 | warm | 0.352 | 0.335 | -4.90% | 1 / 82 | 1 / 82 |
| f64 | cold | 1.028 | 1.024 | -0.41% | 13 / 917 | 13 / 917 |
| f64 | changed_last | 1.016 | 1.018 | +0.17% | 13 / 917 | 13 / 917 |
| matrix_f64_1k | warm | 14.498 | 14.438 | -0.42% | 3 / 14,106 | 2 / 6,106 |
| matrix_f64_1k | cold | 15.234 | 15.636 | +2.64% | 15 / 14,941 | 15 / 14,941 |
| matrix_f64_1k | changed_last | 15.358 | 15.660 | +1.97% | 15 / 14,941 | 15 / 14,941 |
| matrix_f64_100k | warm | 1151.881 | 1140.672 | -0.97% | 3 / 806,106 | 2 / 6,106 |
| matrix_f64_100k | cold | 1152.997 | 1183.009 | +2.60% | 15 / 806,941 | 15 / 806,941 |
| matrix_f64_100k | changed_last | 1147.501 | 1174.567 | +2.36% | 15 / 806,941 | 15 / 806,941 |
| matrix_string_10k | warm | 731.166 | 557.693 | -23.73% | 10,003 / 376,106 | 2 / 6,106 |
| matrix_string_10k | cold | 733.720 | 919.826 | +25.36% | 10,015 / 376,941 | 10,015 / 376,941 |
| matrix_string_10k | changed_last | 748.145 | 925.792 | +23.74% | 10,015 / 376,941 | 10,015 / 376,941 |

The first candidate repeated ordinary numeric conversion during materialization;
its 100k cold regression was about 31%. Consuming the validated grid and only
constructing the Vec substantially reduces that penalty. The table reports the
refined candidate after the fixture provenance fix, not the initial version.

**Decision:** do not replace all formula input conversion with this candidate.
Numeric warm differences are small and cold is slower; numeric warm requested
bytes nevertheless fall by 800,000 for 100k. String-heavy high-hit-rate inputs
are promising: 10k warm removes 10,001 allocations and 370,000 requested bytes,
saving about 173 us; cold costs about 186 us more. A simple weighted
mean predicts a roughly 52% hit-rate crossing for this string fixture
(ignoring workload correlations, factory cost and cache state), not a universal
threshold. Keep the runnable candidate for evaluating representative data before
a breaking public-contract migration. Process B's later string samples were
slower than process A; interpret these short runs as directional evidence.

### RTD topology results

Each value below is the mean of two process medians in milliseconds per round;
each process records six measured rounds after one warmup. These uniform-topic
workloads all passed final-value and update-sequence assertions.

| Subscriptions | Production publishers N | Shared P=1 | Shared P=4 | Worker counts: production / P=1 / P=4 |
| ---: | ---: | ---: | ---: | --- |
| 32 | 6.595 | 4.164 | 7.489 | 64 / 33 / 36 |
| 128 | 30.022 | 16.636 | 29.493 | 256 / 129 / 132 |
| 512 | 156.816 | 53.883 | 67.304 | 1024 / 513 / 516 |

**Decision:** shared publishing is the strongest next production redesign, but
more workers are not automatically faster: P=4 regresses at 32 topics. At 512,
process medians vary substantially (production 132–182 ms; P=1 42–66 ms).
Do not hard-code one publisher from this single-host result. Retain the
experiment until panic-safe publication barriers and skew/fairness tests pass;
do not copy its manually maintained `publishing` flag into production.

### ReadScope and admission results

Fresh 1,000-hit Cache Criterion batches (30 samples, 0.3 s warmup, 1 s requested
measurement) gave the following point estimates. All warm allocation probes
reported zero allocations per batch.

| Cache mode | Run A us | Run B us |
| --- | ---: | ---: |
| current | 29.356 | 29.805 |
| scoped_per_lookup | 40.403 | 40.430 |
| scoped_batch | 35.607 | 35.316 |

**Decision:** do not promote ReadScope. Both current-host measurements lose to
the ordinary pin/lease path; historical 108 vs 114 us numbers do not justify a
new public API now. The additional reentrancy/drain contract cost is unwarranted
by these results. Existing ordinary cache behavior is unchanged.

The scalar admission diagnostic used 30 samples, 0.3 s warmup and the existing
10 s measurement policy, one persistent worker and 1,000 operations per batch:

| Boundary | us per batch |
| --- | ---: |
| Ingress only | 18.187 |
| Full admission | 26.538 |
| Scalar return plus free, no subscriber | 50.473 |

The full-minus-ingress difference is approximately 8.35 ns per operation in
this fixture, including generation-related work and measurement overhead. It
is not the measured saving of a merged implementation. **Decision:** retain
both authorities; no protocol/proof rewrite on this evidence alone.

### Validation

- Workspace all-feature nextest: 973 passed, 11 skipped. After the fixture
  repair, serialized xlfn all-feature libtests: 705 passed, 10 ignored.
- Workspace all-target/all-feature Clippy, formatting, and diff checks passed.
- i686 and x86_64 MSVC all-feature checks with `blake3/pure` passed (compile only).
- Strict-provenance Miri, both Stacked and Tree Borrows: two prepared-value/
  UTF-8 identity tests and two ingress/handle-publication tests passed per model.
- Tests check same-token identity across eager/prepared paths, no warm
  materialization, changed-last-cell publication, rejection of a non-finite or
  invalid-UTF-16 final cell before lookup, integer conversion, signed zero,
  matrix shape and UTF-8 buffer boundaries.

Miri exposed a pre-existing raw-ingress benchmark fixture defect: constructing
raw array pointers before moving Box-backed cells into type-erased storage
invalidated their Stacked Borrows provenance. Fixtures now retain Vec-backed
cell allocations; mutation refreshes raw projections. Final ingress measurements
were rerun with that repair. Native fixture tests run in subprocesses because
the benchmark runtime intentionally lives for the process lifetime and would
otherwise retain diagnostic-service admission across unrelated serialized
lifecycle tests. Miri runs the same bodies directly in its filtered process.

No generated-wrapper migration, production shared pool, public ReadScope, or
admission protocol change is claimed. The decisions above are the outcome of
this experimental pass; the RTD production ownership/panic/fairness work remains
an implementation gate, not a completed optimization.
