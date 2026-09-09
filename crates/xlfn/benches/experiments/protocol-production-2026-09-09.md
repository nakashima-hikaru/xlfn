# Protocol production follow-up — 2026-09-09

Production now keeps inline cache storage, uses one successful Live observation for handle lookup, defers retired bindings with bounded admission debt and eventual maintenance, and publishes RTD channel values with edge wakeups and batches of at most 32. Combined resident/pin state is rejected and archived. Shared RTD publishers and token-cache associativity remain benchmark-only experiments. No kanal replacement or generation-order relaxation was made.

The [initial experiment](protocol-costs-2026-09-09.md) and its prototype patches are historical snapshots. They must not be applied to the current production tree. The follow-up results and full per-run estimates are in [the results JSON](protocol-production-2026-09-09-results.json).

## Handle withdrawal and retirement policy

`remove` success means publication withdrawal, not destructor completion. `read_scoped`'s successful Live observation is the lookup linearization point. A removal immediately after that observation does not invalidate a successful lookup; the existing call permit protects the record and typed projection. A controlled interleaving test withdraws the binding on another thread between the Live observation and projection, then verifies the returned value and rejection of a subsequent lookup. The hook and its storage are both `cfg(test)`; they add no production callback wrapper or reader bookkeeping.

Withdrawal and enqueue into a generation-bound queue share the BindingTable writer lock. Maintenance and arbitrary destructors run after that lock is released. The existing rotating read domain still publishes the next generation before waiting for the sealed old generation. Later readers cannot indefinitely keep the old generation active. This implementation uses that blocking operation on a maintenance worker; it does not introduce a split-phase kernel API.

Retirement registration also synchronizes with next-generation publication.
Rotation holds the old retirement-queue guard through publication and reopening,
then releases it **before** waiting for readers. A writer either registers before
that barrier (making its withdrawal visible to new readers), or acquires the
queue afterward and rechecks into the next generation. A generation recheck
without this publication barrier is insufficient on weak memory: a new reader
could otherwise observe a stale withdrawn pointer whose record was queued to
the old generation. A Loom counterexample rejects that unbarred model; the
barrier model passes. Existing read admission and atomic orderings are unchanged.

Policy:

- One maintenance worker is started lazily on the first publication in each registry. It parks without periodic work while there is no debt, coalesces pending work for approximately 1 ms, then rotates and drains. Even a single retirement is eventually reclaimed without another write or seal, once its readers leave. The coalescing interval is not a scheduling deadline.
- At 32 outstanding records, a remover attempts `try_quiesce_if_idle`; it does not wait for a transition or reader.
- At 256 outstanding records, a remover performs blocking quiescence only when it cannot hold its own default-stripe call permit. The writer checks both generations of its existing stripe. Stripe collisions conservatively defer this wait. There is no new reader-side tracking, Arc clone, or read-permit bookkeeping.
- New binding publication returns `Overloaded` when debt is at least 256. This prevents unbounded retirement even when a remover has its own call permit, collides with another reader, or an arbitrary destructor is still running. Existing bindings remain removable.
- With configured live-binding capacity M, live plus retired records are bounded by M + 256. **256 is a pressure threshold, not a strict maximum retired-queue length.** A scope holding an old reader can reach 256 debt, reject new publication, then withdraw the remaining live bindings. The eight-slot probe reaches 263 retired records and drains to zero. One additional unpublished payload from rejected admission is destroyed separately and is excluded from retirement debt. Pending unpublished objects and async-pinned payloads have their separate lifetimes; this is a binding-record bound, not a byte bound.
- Debt includes destructor batches already transferred out of a queue. Final seal closes admission, drains both generations, joins the worker, and waits for such destruction to complete. Ordinary registry drop also stops and joins maintenance. Async task drain must release pins before final object-quiescence validation.

Bindings and internal pin capabilities now retain the ObjectArena with Arc on writer/pin paths. This is needed when an arbitrary deferred destructor releases the last registry owner on the worker: the arena remains valid through its final release callback. Lookup still uses its cached non-owning object pointer. Reentrant seal from a same-domain reclamation callback returns `Closing`; it cannot join or wait for itself. Destructors run outside table, queue, transition, and worker locks.

The public [handle lifetime guide](../../../../guide/src/handles.md) documents withdrawal semantics and arbitrary destructor threads. Tests formerly assuming synchronous destruction now assert withdrawal while an old borrow remains valid and verify destruction after an explicit test drain or terminal seal.

## Measurement method

Apple M1, macOS 26.6.2, Rust 1.98.0, aarch64-apple-darwin, System allocator. Comparison binaries were compiled first with identical `--all-features` settings, then executed serially without concurrent compilation or tests. Existing user changes in RTD source/catalog work were retained in both variants.

Lookup: 1,000 hits per worker, 700 ms Criterion measurement, 100 ms warm-up, 30 samples. Single versus triple checks uses five alternating pairs; eager versus final production retirement uses five alternating pairs with single-state lookup and the current benchmark harness in both. Tables use the median of independent run medians. These short local results do not establish formal noninferiority or Windows qualification.

| Lookup comparison | Threads | Control µs/batch | Candidate µs/batch | Change |
| --- | ---: | ---: | ---: | ---: |
| Three Live checks → one | 1 | 24.351 | 23.871 | −1.97% |
| Three Live checks → one | 32 | 209.469 | 207.944 | −0.73% |
| Eager → production deferred, both one check | 1 | 24.089 | 22.584 | −6.25% |
| Eager → production deferred, both one check | 32 | 208.292 | 195.687 | −6.05% |

The state-check A/B predates the final test-hook isolation. During final qualification, an intermediate build with a generic test-hook wrapper showed a roughly 3% one-thread regression against a rebuilt eager control. The final lookup restores the direct production body and compiles the interleaving hook only in tests; five matched repeats produced the final values above. Intermediate neutral and regressing runs are retained in the JSON. These small-call measurements are sensitive to code generation and are not universal throughput guarantees.

Removal probes each create 100 fresh registries. Setup is outside the removal timer. The retained-reader case establishes a borrowed handle and requests a 1 ms sleep; actual scheduling and wakeup time are included. All payloads are destroyed exactly once by final drain. Table entries are medians of three independently reported quantiles, not pooled p99 values.

| Reader hold | Policy | Remove p50 µs | Remove p99 µs | Final drain p50 µs | Final drain p99 µs |
| --- | --- | ---: | ---: | ---: | ---: |
| None | Eager | 0.792 | 1.000 | 0.625 | 0.625 |
| None | Production deferred | 0.583 | 1.167 | 14.042 | 22.916 |
| 1 ms | Eager | 1265.542 | 1279.791 | 1.000 | 1.334 |
| 1 ms | Production deferred | 2.750 | 4.500 | 1285.584 | 1306.500 |

The wait moves to final drain. The fresh-registry no-reader case exposes the additional worker-join cost; this is one thread per registry, not per handle or subscription. Arbitrary destructor duration remains outside any bounded-latency promise.

## RTD edge + batch adoption

Production uses an atomic early admission check plus the authoritative mutex-side check, `notify_one` only on empty → nonempty, separate cancellation and publisher Condvars, and at most 32 dequeued values per batch. Close discards queued and locally dequeued values except for a publication already in flight. Finite producer completion still drains accepted values. No source/shutdown ownership contract was relaxed.

The new `RtdChannelPipelineBenchmark` is exposed in both existing `rtd_publish` and `rtd_refresh` benchmarks. It runs persistent sending workers through:

`RtdSender → bounded queue → framework publisher → ErasedSink → PublishCore::publish → refresh planning, collection, and completion`.

Each measured cycle sends 10,000 distinct values per producer, waits for their enqueue completion, enqueues a FIFO marker, and waits until refresh delivers that marker. The marker's publication-sequence delta must equal every accepted distinct update plus the marker. Overload retries use `yield_now` and are reported. Subscription/worker setup and teardown are outside measured cycles. A framework producer waits for cancellation; the sending workers retain safe sender clones.

Five alternating process pairs; each process discards one warm-up cycle and records nine measured cycles. Values below are medians of process medians.

| Producers | Capacity | Original ms | Edge + batch ms | Change |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 64 | 1.700 | 0.513 | −69.81% |
| 1 | 1024 | 1.488 | 0.694 | −53.37% |
| 4 | 64 | 13.053 | 5.975 | −54.22% |
| 4 | 1024 | 11.794 | 5.174 | −56.13% |

All sequence and delivery checks pass. This qualifies the Rust RTD pipeline on this host. It does not measure Excel/COM calls or an actual worksheet refresh on Windows.

## Shared publisher topology experiment

The benchmark-only shared source keeps one producer and a capacity-64 queue per subscription. Ready topics are scheduled to one or four common publishers, which publish up to 32 values before requeueing a topic. Per-topic disconnect closes admission, waits for the local sink clone to be released, and clears the stored sink before returning. Shared workers are joined when their pool is dropped. The candidate is deliberately excluded from normal builds.

Both variants send 1,000 distinct integer updates per subscription through the actual PublishCore and refresh pipeline. All final topic values and per-topic sequence increments are checked. Each process warms once and measures six cycles; three processes reverse configuration order. Worker creation and subscription setup are excluded. Recorded subscription setup time also excludes shared-pool creation, so it must not be used as an end-to-end startup comparison.

| Subscriptions | Current workers | Shared 1 workers | Current ms | Shared 1 ms | Shared 4 ms |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 2 | 0.060 | 0.069 | — |
| 8 | 16 | 9 | 2.478 | 1.248 | 2.246 |
| 32 | 64 | 33 | 8.405 | 4.676 | 5.751 |
| 128 | 256 | 129 | 45.909 | 15.144 | 20.639 |
| 512 | 1024 | 513 | 480.092 | 54.802 | 71.512 |

On this eight-core host, one shared publisher performs best for these uniform small updates at scale. Four publishers are not automatically better. The third 512-topic shared-one process had a 97.422 ms median, compared with about 54.6–54.8 ms in the first two; the raw results retain this variation. One subscription shows a small regression. These results justify a subsequent production design, not immediate promotion: heterogeneous sources, slow/failing publications, fairness, setup failures, cancellation and reentrancy need fuller lifecycle qualification. Producer-per-subscription threads also remain.

## Token-cache associativity experiment

The production direct-mapped cache is unchanged. The [2-way](token-cache-2way.patch) and [4-way](token-cache-4way.patch) archived candidates keep 16 total entries, reducing the number of sets to eight and four respectively. Replacement is round-robin within a set, with eight/four additional replacement indices. Full token bytes, registry address, session, and secret are still checked. These patches are alternatives, not cumulative changes.

The benchmark executes the actual `TokenCodec::parse` authentication/cache path and asserts every returned identity. A fixed authenticated corpus provides natural sequential-slot tokens and deliberately colliding final-tag nibbles. Each case makes 100,000 lookups, discards one warm-up batch, then records six batches. Five processes run the three variants in alternating order. These are controlled token traces, not a sampled worksheet workload.

| Trace | Direct ns/parse | 2-way ns/parse | 4-way ns/parse |
| --- | ---: | ---: | ---: |
| One repeated token | 9.65 | 10.44 | 10.22 |
| Natural 8 tokens | 123.45 | 168.01 | 12.73 |
| Natural 16 tokens | 147.62 | 169.37 | 134.05 |
| Natural 32 tokens | 281.57 | 332.65 | 337.26 |
| Two tokens in one bucket | 298.33 | 11.31 | 17.20 |
| Four tokens in one bucket | 302.17 | 309.51 | 14.31 |
| Eight tokens in one bucket | 310.42 | 318.00 | 323.11 |

Associativity avoids repeated authentication when a colliding working set fits in its ways. Reducing set count can create new conflicts: the natural eight-token trace has tag nibbles `88ae8d30`, so two ways still overflow the shared set for `8` and `0`, whereas four ways fit it. The one-token path is approximately 6–8% slower and the natural 32-token case approximately 18–20% slower. Keep direct mapping until representative per-call token traces justify the tradeoff.

## Validation and reproduction

Final validation: xlfn all-feature tests **595 passed, 7 ignored**, UI compile suite passed; kernel **58 passed**; all-target/all-feature Clippy for xlfn and xlfn-kernel passed with warnings denied; the panic-boundary audit passed with 44 reviewed direct references and all five audit tests. Core-only checking and Windows x86 all-feature type checking pass. Windows x64 type checking passes with `blake3/pure`; its normal assembly build requires `ml64.exe`, unavailable on this Mac. These are type checks, not Windows runtime or Excel/COM validation. Kernel tests include the positive and expected-failure Loom publication-barrier models, barrier release before the reader wait, and conservative caller-stripe detection. Handle tests cover own-scope hard pressure, a hard-pressure remover that must wait for another reader, eventual single-retirement collection, slot reuse, concurrent seal/removal, in-flight destructor completion, last-owner destruction on the worker, and lookup linearization during withdrawal. RTD tests cover wakeups with cancellation waiters, FIFO batches, finite multi-batch drain, in-flight cancellation, disconnect joins, and panic containment.

Miri passes for the deferred-retirement scenarios in both Stacked Borrows and Tree Borrows, and for the three RTD batch/cancellation scenarios in both models. Arena pin lifecycle also passes both models; the final lookup interleaving passes both models, and the kernel stripe test passes Stacked Borrows. The parking_lot dependency reports exposed-provenance integer-to-pointer casts during contended waits, so these runs do not constitute strict-provenance qualification. The earlier full-Moka Miri limitation is unchanged.

The first all-feature parallel run in this follow-up failed the unchanged `failed_controlled_reload_quarantines_the_runtime` test; it passed in isolation, in the serial suite, and in the final parallel run. No lifecycle source changes were made for that failure.

Use the normal benchmark commands for the new integrated pipeline:

```sh
XLFN_BENCH_MEASUREMENT_MS=1000 cargo bench -p xlfn --all-features \
  --bench rtd_publish -- channel_pipeline --noplot --warm-up-time 0.1 --sample-size 30
XLFN_BENCH_MEASUREMENT_MS=1000 cargo bench -p xlfn --all-features \
  --bench rtd_refresh -- channel_pipeline --noplot --warm-up-time 0.1 --sample-size 30
cargo bench -p xlfn --all-features --bench protocol_costs -- --pipeline
cargo bench -p xlfn --all-features --bench protocol_costs
XLFN_TOPOLOGY_SUBSCRIPTIONS=512 XLFN_TOPOLOGY_PUBLISHERS=1 \
  cargo bench -p xlfn --all-features --bench protocol_costs -- --topology
cargo bench -p xlfn --all-features --bench protocol_costs -- --token-cache
```

For topology, publisher count `0` selects the production per-subscription adapter; `1` and `4` select the benchmark-only shared pool. Repeat subscription counts `1, 8, 32, 128, 512`. For token-cache alternatives, use a disposable checkout, apply one archived patch, build its executable, restore direct mapping, then compile the other variant. Save all comparison executables before timing them serially. Default Criterion runs retain the normal ten-second measurement setting when the override is absent.
