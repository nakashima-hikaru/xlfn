# Resident index qualification

## Protocol fixed before implementation and measurement

Baseline: `5de48a5`. The production backend initially remains Moka. Both
candidates must use the same `CalculationCache`, node, pin, read-domain,
generation, retirement, and reclamation logic. Only resident-set selection
and indexing may differ. No change to those lifetime protocols is authorized
as part of this experiment.

Candidates are Moka and a fixed sharded map with 8, 16, 32, or 64 shards.
Weighted capacity is global, not a separate quota per shard: skewed keys must
not silently reduce the configured budget. The simple candidate policy may
choose arbitrary entries from rotating shards, without updating policy on a
read. Concurrent initialization of the same versioned key must remain
single-flight. Application initialization and eviction callbacks run outside
the candidate's map/policy locks.

### Implemented boundary

`CalculationCache` owns a private `ResidentIndex` instead of a Moka cache.
The adapter contains the Moka builder, equivalent-key lookup, single-flight
initializer and maintenance/invalidation APIs. The sharded alternative stores
the identical node-pointer/weight tuple and serializes global weighted-capacity
decisions with one writer policy mutex. Reads lock only the selected shard.
Removed entries are collected under locks, then passed to the same xlfn
retirement callback after the sharded policy/map locks are released.

The callback's existing residency-store, pin-release and retirement-enqueue
body was extracted without changing ordering. Node allocation, creator/lease
pins, scoped read admission, generation handling, backpressure and domain
quiescence/reclamation remain the shared implementation. A separate per-key
flight map prevents duplicate initialization; application initialization runs
outside the index locks, and an unwinding initializer wakes followers to retry.

### Correctness gates

All backend-common regressions must pass: live leases during eviction,
concurrent clear/lookups, generation rollover, duplicate initialization,
reentrancy, exactly-once destruction, retirement-debt drain, weighted bounds,
and concurrent reads/writes. Existing Moka regressions must remain green.

The sharded backend must pass full-cache ownership regressions under both
Stacked Borrows and Tree Borrows, with Miri validation and leak checking
enabled. These execute the actual cache/index/node/reclamation code, including
scoped-reference eviction and reduced clear/concurrency workloads. A model
of the kernel alone does not qualify the cache. No ignored leaks or disabled
aliasing validation may be used to turn a failure into a pass.

### Performance and debt gates

Use the same keys, capacities, payloads, task counts, worker topology, and
cache operations for both backends. Collect latency distributions separately
from uninstrumented throughput timings. Run candidates serially on the same
host. Use three timing repetitions per case; report medians and retain the
individual observations. Smoke runs only validate the harness.

For a production switch, one fixed shard count must satisfy all of:

- Warm, hot 1/8/32-thread and disjoint 8/32-thread batch time: no case more
  than 10% slower than Moka; geometric mean slowdown at most 5%.
- Read-heavy mixed workload at 32 threads: throughput at least 90% of Moka;
  writer p99 no more than 25% higher. Report hit p50/p95/p99 separately; no
  repeatable hit-tail increase over 10% is acceptable.
  Use one fresh-key write per 32 operations per worker, with remaining reads
  of a hot key. Record the hit rate as well: it must be at least 99% of Moka's
  rate so faster misses cannot masquerade as faster hits.
- Eviction, clear/invalidate, and eviction with live leases: throughput at
  least 90% of Moka. A favorable warm lookup cannot hide a write-path loss.
- Peak queued retirement nodes and weight must not exceed Moka's maximum
  across matching repetitions. Debt must drain to zero after readers/leases
  leave and explicit maintenance completes. Resident weight must be within
  budget after maintenance for both backends.
- Report resident entry counts, approximate memory footprint, and shard-count
  sensitivity. Distinguish resident memory, live-lease retention, and queued
  retirement debt. Estimates are not process RSS.

The decision uses all gates, not the fastest isolated row. If no candidate
qualifies, retain Moka in production and retain the sharded implementation
only for tests/benchmarks. Record Moka's outstanding dependency-blocked Miri
qualification in `cache-miri.md`; rerun that qualification only when Moka,
Crossbeam, Miri, or nightly changes.

### Dependency accounting

Before any switch, inspect `cargo tree` for both cache-only and all-feature
configurations. At baseline, `crossbeam-epoch` is reached through both Moka
and the async executor's `crossbeam-deque`. Removing Moka alone therefore
does not remove the async dependency path. Keep those observations separate
and do not claim workspace-wide removal from a cache-only check.

### Timing implementation fixed before runs

The supplemental `cache_backends` executable uses persistent workers, 256
operations per worker per batch, 64 resident entries, 8-byte u64 values,
a 200 ms warmup and a 1 second timing window per repetition. Three separate
latency batches follow each timing window. Per-operation clocks and debt
observations are confined to the latency probe. Backend order rotates across
three repetitions. Existing Criterion lookup and reclamation harnesses are
also run for all backends; raw output is retained. The existing lookup harness
compares its `current` variants and the scoped clear probe; its diagnostic
no-admission/no-pin/Arc controls remain Moka-specific and are excluded from
qualification timing. Criterion uses 20 samples, a 200 ms warmup, and a
1 second target measurement window (long batches can exceed that target).

## Reproduction and evidence

- Native timings: `rustc 1.98.0 (88d9e12ae 2026-08-18)`, LLVM 22.1.8,
  `aarch64-apple-darwin`, release profile, `Cargo.lock` retained.
- Run `just bench-cache-backends`, then
  `python3 -B tools/summarize_cache_backends.py target/cache-backend-qualification`.
- `cargo test --workspace --all-features --locked -- --test-threads=1`:
  765 tests passed, 7 pre-existing ignored tests; doctests passed.
- `cargo clippy --workspace --all-targets --all-features --locked`: passed.
- The original 51 Moka cache regressions passed after the adapter extraction.
  Nine additional common tests run all five backends natively and all four
  shard counts under Miri. See `cache-miri.md` for both borrow-model results.
- The production build has only the Moka enum variant. Sharded selection is
  guarded by `bench-internals`; its optional hashbrown dependency is not
  enabled by `unstable-cache` alone.

Dependency checks (`cargo tree -p xlfn -e normal -i crossbeam-epoch --locked`):
`--no-default-features --features unstable-cache` reaches `crossbeam-epoch
0.9.20` via `moka 0.12.16`; `--all-features` also reaches it through
`crossbeam-deque 0.8.7`. A cache backend change alone cannot remove the async
executor's dependency path.

## Measured results (2026-09-07)

Host: Apple M1, 8 logical CPUs, 16 GiB RAM, aarch64-apple-darwin. The 32-thread cases oversubscribe this host. Three repetitions; each table reports the median unless labeled maximum. These short local windows support this host's decision, not a universal ranking.

**Decision: retain Moka in production.**

| Shards | Worst lookup time / Moka | Geometric mean time / Moka | Qualifies |
|---|---:|---:|---|
| 8 | 1.001 | 0.401 | False |
| 16 | 1.169 | 0.368 | False |
| 32 | 0.962 | 0.334 | False |
| 64 | 1.173 | 0.333 | False |

Failures by candidate:

- sharded8: mixed32 throughput below 90%; mixed32 writer p99 above 125%; mixed32 median hit latency/tail above 110%; mixed32 hit rate below 99% of baseline; supplemental write/clear throughput below 90%; existing eviction/clear/reclamation throughput below 90%; peak retirement debt exceeds baseline.
- sharded16: lookup case slowdown exceeds 10%; mixed32 throughput below 90%; mixed32 median hit latency/tail above 110%; supplemental write/clear throughput below 90%; existing eviction/clear/reclamation throughput below 90%; peak retirement debt exceeds baseline.
- sharded32: mixed32 throughput below 90%; mixed32 median hit latency/tail above 110%; mixed32 hit rate below 99% of baseline; supplemental write/clear throughput below 90%; existing eviction/clear/reclamation throughput below 90%; peak retirement debt exceeds baseline.
- sharded64: lookup case slowdown exceeds 10%; mixed32 throughput below 90%; mixed32 median hit latency/tail above 110%; mixed32 hit rate below 99% of baseline; supplemental write/clear throughput below 90%; existing eviction/clear/reclamation throughput below 90%; peak retirement debt exceeds baseline.

### Existing Criterion workloads

Batch-time ratios below 1 favor the candidate. Moka time is the median of three mean estimates in microseconds. Reclamation IDs encode payload bytes and worker counts.

| Case | Moka µs/batch | N8 / Moka | N16 / Moka | N32 / Moka | N64 / Moka |
|---|---:|---:|---:|---:|---:|
| cache_hit/u64/current/warm | 114.41 | 0.302 | 0.303 | 0.302 | 0.302 |
| cache_hit_disjoint/current/threads_32/u64 | 4024.67 | 0.302 | 0.176 | 0.138 | 0.106 |
| cache_hit_disjoint/current/threads_8/u64 | 1008.39 | 0.168 | 0.140 | 0.136 | 0.123 |
| cache_hit_hot_key/current/threads_1/u64 | 114.44 | 0.303 | 0.301 | 0.301 | 0.301 |
| cache_hit_hot_key/current/threads_32/u64 | 6631.14 | 0.890 | 0.953 | 0.852 | 0.976 |
| cache_hit_hot_key/current/threads_8/u64 | 1414.27 | 1.001 | 1.169 | 0.962 | 1.173 |
| concurrent_clear_latency/scope_100 | 25.83 | 0.603 | 0.609 | 0.608 | 0.608 |
| eviction_with_live_lease/current | 730.31 | 0.215 | 0.221 | 0.236 | 0.260 |
| churn/payload_64b/threads_1 | 374.24 | 0.414 | 0.410 | 0.410 | 0.409 |
| churn/payload_64b/threads_32 | 11985.27 | 1.486 | 1.517 | 1.642 | 1.643 |
| churn/payload_64b/threads_8 | 2965.22 | 0.946 | 0.965 | 0.975 | 0.967 |
| churn/payload_65536b/threads_1 | 653.70 | 0.865 | 0.909 | 0.881 | 0.890 |
| churn/payload_65536b/threads_32 | 17211.60 | 1.477 | 1.508 | 1.619 | 1.562 |
| churn/payload_65536b/threads_8 | 4269.86 | 0.920 | 0.904 | 0.908 | 0.912 |
| live_leases/payload_64b/threads_1 | 461.61 | 0.372 | 0.369 | 0.374 | 0.374 |
| live_leases/payload_64b/threads_32 | 30930.08 | 0.810 | 0.813 | 0.801 | 0.801 |
| live_leases/payload_64b/threads_8 | 6237.49 | 0.718 | 0.716 | 0.716 | 0.724 |
| live_leases/payload_65536b/threads_1 | 725.50 | 0.812 | 0.855 | 0.832 | 0.862 |
| live_leases/payload_65536b/threads_32 | 33770.40 | 0.908 | 0.909 | 0.901 | 0.899 |
| live_leases/payload_65536b/threads_8 | 7018.22 | 0.884 | 0.888 | 0.887 | 0.882 |

### Supplemental throughput

Ratios above 1 favor the candidate. Each operation includes lease release or replacement; retained lease buffers have 32 slots per worker. Latency clocks and statistics observation are excluded from these timing runs.

| Case | Moka Mops/s | N8 / Moka | N16 / Moka | N32 / Moka | N64 / Moka |
|---|---:|---:|---:|---:|---:|
| Clear 1T | 0.179 | 7.875 | 7.607 | 7.088 | 6.183 |
| Disjoint 8T | 7.965 | 2.886 | 3.132 | 3.396 | 3.487 |
| Disjoint 32T | 6.289 | 3.097 | 4.566 | 4.959 | 5.784 |
| Eviction 1T | 0.684 | 2.374 | 2.421 | 2.497 | 2.519 |
| Eviction 32T | 0.710 | 0.633 | 0.621 | 0.540 | 0.564 |
| Hot 1T | 7.643 | 2.927 | 2.933 | 2.930 | 2.930 |
| Hot 8T | 5.393 | 1.292 | 1.303 | 1.309 | 1.308 |
| Hot 32T | 5.183 | 1.070 | 1.071 | 0.954 | 1.072 |
| Invalidate 1T | 0.453 | 3.643 | 3.577 | 3.579 | 3.658 |
| Invalidate 32T | 0.182 | 1.886 | 1.765 | 1.968 | 1.907 |
| LiveEviction 1T | 0.583 | 2.493 | 2.565 | 2.664 | 2.729 |
| LiveEviction 32T | 0.269 | 1.219 | 1.230 | 1.233 | 1.233 |
| Mixed 1T | 5.456 | 2.089 | 2.003 | 1.945 | 2.348 |
| Mixed 32T | 4.111 | 0.454 | 0.779 | 0.585 | 0.774 |

### Read-heavy 32T tails and writer contention

One fresh write per 32 worker operations; other operations read key 0 and refill after misses. Hit rate counts misses before refill. Latencies are microseconds; p99 writer contention compares the 32T and 1T mixed probes. Three latency batches per repetition are separate from throughput.

| Backend | Hit rate | Hit p50 / p95 / p99 µs | Writer p50 / p95 / p99 µs | Writer p99 32T / 1T |
|---|---:|---|---|---:|
| moka | 100.0000% | 1.000 / 2.333 / 4.167 | 5.083 / 512.500 / 1016.917 | 223.9 |
| sharded8 | 93.3048% | 0.666 / 2.833 / 5.542 | 4.041 / 331.000 / 2129.917 | 2044.1 |
| sharded16 | 99.6724% | 1.000 / 3.291 / 5.333 | 2.500 / 353.250 / 936.375 | 864.6 |
| sharded32 | 97.4840% | 1.000 / 3.250 / 5.292 | 3.042 / 313.750 / 753.958 | 754.0 |
| sharded64 | 97.4126% | 0.791 / 2.875 / 5.584 | 3.167 / 412.417 / 1130.875 | 798.1 |

### Retirement debt and storage

Maximum queued nodes across three repetitions, measured before final clear. Weights are eight times the node counts in this u64 matrix. Counts exclude resident nodes and nodes still pinned by live leases. All final pending-node/weight counts were zero after readers and leases exited and maintenance completed.

| Case | Moka | N8 | N16 | N32 | N64 |
|---|---:|---:|---:|---:|---:|
| Clear 1T | 1 | 1 | 1 | 1 | 1 |
| Disjoint 8T | 0 | 0 | 0 | 0 | 0 |
| Disjoint 32T | 0 | 0 | 0 | 0 | 0 |
| Eviction 1T | 32 | 1 | 1 | 1 | 1 |
| Eviction 32T | 436 | 73 | 77 | 77 | 78 |
| Hot 1T | 0 | 0 | 0 | 0 | 0 |
| Hot 8T | 0 | 0 | 0 | 0 | 0 |
| Hot 32T | 0 | 0 | 0 | 0 | 0 |
| Invalidate 1T | 1 | 1 | 1 | 1 | 1 |
| Invalidate 32T | 64 | 72 | 83 | 73 | 74 |
| LiveEviction 1T | 32 | 1 | 1 | 1 | 1 |
| LiveEviction 32T | 53 | 31 | 31 | 29 | 32 |
| Mixed 1T | 2 | 1 | 1 | 1 | 1 |
| Mixed 32T | 226 | 77 | 83 | 85 | 86 |

The controlled scoped-reference clear probe queued exactly 64 nodes / 512 weighted bytes for every backend, then drained to zero after releasing the reader.

Storage estimates below use the mixed 32T cache after maintenance. Moka index storage is a lower bound excluding opaque policy/table metadata; sharded estimates include shard headers, hash-table capacity/control-byte estimates and flight-map capacity. Neither includes allocator rounding, heap storage in keys, or process RSS. Resident node bytes include caller weights and headers. Retained payload estimates overlap resident storage when a lease is still resident, so these columns must not be summed as distinct allocations.

| Backend | Resident entries | Weight | Index estimate bytes | Resident node estimate bytes | Live-eviction 32T held payload upper bound |
|---|---:|---:|---:|---:|---:|
| moka | 64 | 512 | 2104 | 3072 | 8192 |
| sharded8 | 64 | 512 | 12809 | 3072 | 8192 |
| sharded16 | 64 | 512 | 26094 | 3072 | 8192 |
| sharded32 | 64 | 512 | 29894 | 3072 | 8192 |
| sharded64 | 64 | 512 | 28518 | 3072 | 8192 |

Raw `criterion.jsonl` retains all 300 timing estimates and 180 allocation/latency/reclamation probes (including 64 B and 64 KiB payloads at 1/8/32T). `supplemental.jsonl` retains all 225 observations with hit/write p50/p95/p99, resident counts, debt and estimates. `decision.json` retains every failed gate and debt comparison. Console logs remain in the run directory; raw data copied beside this report is sufficient to recompute these tables.


### Production disposition

Keep Moka as the production default and preserve its feature/dependency and
Moka diagnostic benchmark support. Sharded remains a test/benchmark baseline.
All four shard counts miss the read-heavy 32T throughput gate (45.4–77.9% of
Moka), and 32T eviction reaches only 54.0–63.3% of Moka. Invalidate 32T also
raises the maximum queued debt from 64 nodes to 72–83 nodes. These failures
are sufficient to reject a switch despite faster warm/disjoint reads and
successful sharded Miri runs. No node lifetime or reclamation-policy change
was made to improve the candidate's scores.

The raw data is in [resident-index-results](resident-index-results/).
Recompute the decision and tables with:

```sh
python3 -B tools/summarize_cache_backends.py \
  crates/xlfn/benches/experiments/resident-index-results
```

Moka's production full-cache Miri qualification remains dependency-blocked;
[cache-miri.md](cache-miri.md) records the limitation and the update-only
requalification policy. Since the candidate did not qualify, no Moka or
Crossbeam dependency was removed and no async executor rewrite was needed.

## Production abstraction codegen review (2026-09-07)

The existing single-variant production representation is zero-cost for the
checked `u64` key/value instantiations. Keep `CalculationCache -> ResidentIndex
-> MokaResidentIndex -> moka::Cache`: no new generic backend parameter, trait
object, or production backend-selection feature is needed. The sharded Box and
second enum variant are compiled only with `bench-internals`, including qualification tests.

A disposable source copy was compiled as a **release library**, with
`unstable-cache` enabled and **without test or bench-internals**. The
[diagnostic probe](resident-index-codegen-probe.rs) compares calls through the
actual resident wrapper with calls on the actual Moka cache type, and asserts
equal size/alignment at compile time. No probe is compiled into normal library
or benchmark builds. Rustc 1.98.0 / LLVM 22.1.8, one codegen unit, no LTO override:

| Target | ResidentIndex and Moka size / alignment | get / insert / invalidate | clear / count / weight |
|---|---|---|---|
| aarch64-apple-darwin | 56 / 8 bytes | Identical assembly | Same LLVM function aliases |
| x86_64-pc-windows-msvc | 56 / 8 bytes | Identical assembly | Same LLVM function aliases |
| i686-pc-windows-msvc | 28 / 4 bytes | Identical assembly | Same LLVM function aliases |

For example, both AArch64 lookup probes load the borrowed key's two words,
move one argument, then tail-branch directly to the same Moka `get`
instantiation. Neither loads a backend tag, adjusts the index address,
dereferences another index pointer, nor calls a wrapper/vtable function.
`insert` and `invalidate` likewise reach exactly the same Moka routines.

Maintenance assembly also matches on AArch64. On Windows x64 the wrapped
version has one fewer register move; on x86 only register allocation differs.
Those Windows maintenance bodies are not byte-identical, but contain the same
Moka housekeeping/lock control flow with no added backend dispatch or object
indirection. [codegen.json](resident-index-results/codegen.json) retains layout
results, normalized assembly hashes/instruction counts and lookup snippets.
Only local block numbers and assembler directives/comments were excluded from
assembly comparison; loads, branches, calls and their destinations were kept.

Windows results are cross-compiled codegen checks, not Windows runtime tests.
`blake3/pure` was enabled only in the disposable Windows builds to avoid a
host C toolchain requirement; it does not change the resident index or Moka.
These observations cover the stated compiler/targets/instantiations, rather
than promising identical output for all future compiler versions or key types.

The feature graph was also checked with every production feature enabled:

```sh
cargo tree -p xlfn --no-default-features \
  --features "unstable-cache async rtd handles unstable-output refinement" \
  -e normal,no-proc-macro --locked
```

There is no hashbrown dependency in that target runtime graph. The unpruned
graph can still contain the pre-existing host-side path
`xlfn-macros -> proc-macro-crate -> toml_edit -> indexmap -> hashbrown`; this
is separate from the benchmark-only resident backend dependency.

### Reproducing the codegen review

From the repository root, create a disposable copy and append the probe:

```sh
python3 - <<'PYCODE'
from pathlib import Path
import shutil
root = Path.cwd()
copy = root / "target/resident-codegen-audit/workspace"
copy.mkdir(parents=True, exist_ok=True)
for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]:
    shutil.copy2(root / name, copy / name)
for name in ["crates", "tools/windows-bindings", ".cargo"]:
    shutil.copytree(root / name, copy / name, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("target", ".git"))
index = copy / "crates/xlfn/src/cache/resident_index.rs"
probe = root / "crates/xlfn/benches/experiments/resident-index-codegen-probe.rs"
index.write_text(index.read_text() + "\n" + probe.read_text())
PYCODE

CARGO_TARGET_DIR="$PWD/target/resident-codegen-audit/build" \
cargo rustc --manifest-path target/resident-codegen-audit/workspace/Cargo.toml \
  -p xlfn --release --no-default-features --features unstable-cache \
  --lib --locked -- --emit=llvm-ir,asm -C codegen-units=1
```

For Windows, repeat the cargo command with
`--target x86_64-pc-windows-msvc` or `--target i686-pc-windows-msvc` and
`--features "unstable-cache blake3/pure"`. Inspect the paired
`resident_codegen_*_direct` / `resident_codegen_*_wrapped` functions in the
emitted `.s` files and function aliases in `.ll`. The normal source tree is
never patched with the probe. This is an on-demand backend/toolchain review,
not another daily Moka Miri job. The existing Miri blocker policy, sharded
qualification baseline, raw benchmark data and production Moka decision remain
in force; no sharded policy tuning was undertaken.

Post-review validation: the 60 cache tests (including all backend-common
regressions) passed; production-feature-only library Clippy, formatting and
`git diff --check` passed. The only executable-source cleanup was removing an
unused binding from the sharded no-op maintenance arm. The measured production
hot path and the sharded policy were not changed.

## Qualification closure

The resident-index experiment is complete with the production decision fixed
at Moka. Preserve the current abstraction, sharded qualification baseline,
benchmark harnesses, summarizer, decision JSON and raw evidence. Do not promote
a sharded production feature, add backend traits/generics, or tune its policy
as continuation of this experiment. New cache-backend work requires a separate
objective; the outstanding follow-up here is full-cache Moka Miri
requalification only after Moka, crossbeam-epoch, Miri or nightly changes.
The codegen probe stays a disposable, on-demand audit.

The final API/feature/plumbing review found every exported benchmark hook has
a consumer. Their public visibility is required by the separate benchmark
crates and remains gated by `bench-internals`. `MokaResidentIndex` is private
to its module. Sharded definitions and backend selection now require
`bench-internals` even in unit-test builds; the common qualification tests
already require that feature. The redundant hashbrown dev-dependency was
removed, so ordinary cache tests also retain the Moka-only representation.
No runtime algorithm or measured qualification configuration changed.

Final cleanup validation: 46 ordinary Moka cache tests passed without
`bench-internals`; 60 cache tests passed with it, including all common backend
regressions. All-target/all-feature xlfn Clippy and formatting passed. Cargo
metadata confirms no hashbrown/sharded production feature and no codegen-probe
build target. The measured configurations are unchanged, so no additional
benchmark or Moka Miri run was needed.
