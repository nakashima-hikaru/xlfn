# RTD multiplexed source implementation and local validation

The implementation adds a safe shared-connection adapter before freezing the RTD API. It separates topic ownership from producer execution and bounds source/runtime resources. No version or publication change is included.

## Contracts

- `RtdMultiplexedSource<T>` owns one lazy producer worker for a shared connection. Topic subscriptions own revocable channels, with no producer thread per topic. A completed source is terminal; its worker slot stays reserved until join.
- Subscription disconnect revokes its channel without waiting for upstream I/O. Runtime shutdown seals admission, requests every source to stop, drains subscription barriers, joins source workers, and then stops shared publication. User I/O must cooperate with cancellation.
- Subscribe/removal events use distinct source-local IDs. Admission reserves room for both events. A source slot stays reserved until its removal is consumed, so rapid reconnects cannot bypass a stalled control queue.
- `RtdDelivery::Fifo` retains FIFO admission; `Latest` replaces only the pending value. Failed replacement preserves the previous value. Terminal errors use a separate fixed slot. Neither mode promises Excel displays every intermediate update.
- Source and runtime budgets cover queued and publishing value counts and logical bytes. One queue ledger transfers ownership to one ledger per publication batch. Payloads are disposed before their reservations return. Byte charges exclude spare queue allocation, allocator overhead, converter temporaries, upstream buffers and Excel storage.
- The default runtime adapter-worker limit is now 64 across sources; existing applications needing more must configure `RtdLimits::with_max_workers`. Each source defaults to 4,096 retained subscriptions, 4,096 buffered values and 64 MiB of buffered-value charges. The runtime also defaults to 4,096 buffered values and 64 MiB. Direct custom unsafe sources are outside adapter-worker/buffer accounting.

Independent review led to fixes for cancellation before admission drain, first-topic registration before producer startup, failed-spawn capture disposal outside internal locks, and publication/drop accounting.

## Paired FIFO pipeline measurement

Baseline: `5bd23bb1c73c00bcc7ae9b60f0e231c9c7ef3ee6`. Candidate: the working tree with aggregate accounting. Host: `macOS-27.0.1-arm64-arm-64bit-Mach-O`; `rustc 1.99.1 (58cf8f9c3 2026-10-13)`.

Six alternating baseline/candidate trials used separate immutable optimized executables. Each trial contains nine measured rounds after a warm-up, with 10,000 updates per producer. The table reports the median of trial medians. The production sender → channel → publisher → RTD storage → refresh path checks the final sequence; these are local Rust timings.

| Producers | Queue capacity | Baseline ms | Candidate ms | Elapsed change |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 64 | 0.498 | 0.593 | +19.0% |
| 1 | 1024 | 0.553 | 0.640 | +15.8% |
| 4 | 64 | 4.061 | 5.324 | +31.1% |
| 4 | 1024 | 3.452 | 4.970 | +44.0% |

The stricter accounting still costs time in this hot-loop workload. The initial per-value permit implementation regressed elapsed time by 118–186%; it was replaced with aggregate queue/batch accounting. Its raw measurements and hashes remain under `rejected_per_value_accounting` in the JSON. No update-throughput gain is claimed.

## Idle subscription lifecycle

Five candidate trials compare both adapters in the same executable. These cases publish no data and make no Excel/COM calls. Producer counts are job-start counters backed by one framework thread per job, excluding publisher, cleanup and notification infrastructure. Timings below are medians of synchronous runtime calls; separate startup/event-consumption wait timings are retained in the raw data.

| Adapter | Topics | Producer jobs | Connect ms | Disconnect ms | Close ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| blocking | 1 | 1 | 0.063 | 0.024 | 0.036 |
| multiplexed | 1 | 1 | 0.039 | 0.003 | 0.047 |
| blocking | 32 | 32 | 0.277 | 0.502 | 0.032 |
| multiplexed | 32 | 1 | 0.061 | 0.010 | 0.046 |
| blocking | 64 | 64 | 0.488 | 0.950 | 0.033 |
| multiplexed | 64 | 1 | 0.078 | 0.028 | 0.042 |
| multiplexed | 512 | 1 | 0.399 | 0.217 | 0.043 |
| multiplexed | 2,048 | 1 | 1.429 | 0.942 | 0.042 |
| multiplexed | 4,096 | 1 | 2.687 | 1.970 | 0.053 |

Every trial asserted that all started jobs finished and zero producer jobs remained after runtime close. A separate native regression delivered and checked values for 2,048 topics with a runtime worker limit of one. No RSS or thread-stack memory measurement was taken.

## Reproduction and provenance

Build the baseline and candidate in separate directories, copy each executable to an immutable path, and alternate these commands:

```sh
CARGO_INCREMENTAL=0 cargo bench -p xlfn --features 'bench-internals rtd' --bench protocol_costs --no-run --locked
<executable> --pipeline
<candidate-executable> --rtd-topology
```

The accompanying [JSON](rtd-multiplex-2026-10-26.json) contains raw rounds, pair order, executable SHA-256 values, hashes of RTD source inputs, and the earlier rejected implementation. The candidate retains unrelated pre-existing working-tree changes; no commit is implied.

## Qualification boundary

Passed checks:

- Final all-feature native library tests: 1,061 passed, 8 ignored, serialized with `--test-threads=1`.
- Focused public source/topic/runtime-config tests: 11 passed. Existing integration/UI/doctests also passed during implementation, including typed retry checks.
- Final RTD Miri selection: 11 passed under Stacked Borrows and 11 under Tree Borrows, using `nightly-2026-09-02` and `-Zmiri-disable-isolation`. This covers queue/batch accounting and a sender surviving runtime destruction.
- Workspace all-target/all-feature Clippy with warnings denied; Rustdoc with warnings denied; formatting and diff checks; panic-boundary audit (52 reviewed direct references).
- Core-only and handles-only feature checks; RTD production library checks for Windows x86 and x64.
- RTD guide examples (2 doctests and the standalone project), guide validation (17 chapters), and the standalone example's Windows x64 cross-check.

macOS cross-compilation does not establish Windows execution, COM scheduling, Excel unload behavior, or feed latency under real workloads. Windows all-target checking was additionally attempted but the local build of the dev dependency `alloca` lacks the Windows SDK `malloc.h`; production library cross-checks are separate evidence.

Before a 1.0 release decision, evaluate the remaining FIFO cost on representative feeds and run matching Windows/Excel lifecycle and update tests.
