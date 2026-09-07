# Historical backend-only qualification: retain Moka (C)

This verdict applied to the original backend-only boundary. The later
[production decision](resident-index-qualification.md) adopts Quick Cache with
xlfn shared flights after explicitly accepting the measured performance tradeoffs.

On 2026-09-07, Quick Cache 0.7.0 was rejected at the protocol compatibility
gate. Moka 0.12.16 was retained at that stage. No performance
gate was evaluated; this is not a claim that Quick Cache is slower.

## Blocking difference: failed single-flight initialization

The temporary adapter used the same `(NodePtr<V>, u32)` entry, borrowed hit
lookup, native `get_value_or_guard`, and the unchanged xlfn initializer and
retirement callback. A two-thread full-cache probe held the leader initializer
until a follower started a same-key lookup, then returned `XllError::Closing`.
The follower's initializer, if executed, returned `Ok(42)`.

| Observation | Moka | Quick Cache |
| --- | --- | --- |
| Leader result | Error | Error |
| Waiting follower result | Same flight's error | Success (42) |
| Follower initializer calls | 0 | 1 |
| Follower retries successfully after leader panic | Yes | Yes |

Both probes were run once, then repeated three times per backend with identical
results on aarch64 macOS, Rust 1.98.0. The probe used a 250 ms scheduling window,
so it is observational rather than a deterministic scheduler proof. Upstream
code independently explains the difference: Moka coalesces fallible results;
Quick Cache's uninserted guard wakes a waiter to take over initialization.

With the required entry type, Quick Cache's guard cannot publish an error to
the waiting calls. Preserving Moka's observable behavior would require additional
shared error/flight state or a different stored value representation. Those
adaptations are outside the requested boundary. Consequently qualification
stopped under option C, without changing the lifetime/reclamation protocol.

## Evidence before stopping

- The 11 existing common cache protocol tests passed natively and under both
  Stacked Borrows and Tree Borrows for Quick Cache. Miri used the same reduced
  bounds as the earlier sharded experiment, `-Zmiri-disable-isolation`, and
  additionally `-Zmiri-tree-borrows` for TB; leak and alias checking stayed on.
- Miri (Rust nightly 1.100.0, c656540d6) warned about an integer-to-pointer cast
  in `parking_lot_core` 0.9.12, `word_lock.rs:320`. Passing these tests does not
  remove that diagnostic or prove the complete dependency path free of UB.
- `Lifecycle::RequestState::drop` invoked the existing retirement callback after
  unlocking. Oversized rejection used existing creator/residency pins, with no
  new lifetime states. Panic follower recovery passed; error sharing did not.
- Default native sharding exceeded an exact weight budget (63 permitted 64).
  A fixed native one-shard candidate passed the added capacity-bound probe;
  no xlfn quota or eviction policy was introduced.
- A supplemental harness smoke check ran, but formal Criterion/supplemental
  repetitions, performance/debt gates, comparative memory measurements, and
  Windows candidate qualification were not performed after the blocker.

The temporary adapter, dependency/lockfile additions, selection feature,
qualification probes, restored benchmark harness, and runner were removed.
Production code and its test bounds are unchanged from the pre-experiment
baseline at that stage. Its runtime cache dependency path still included Moka
and `crossbeam-epoch`; see the [historical Miri limitation](cache-miri.md).

Sources inspected: [Moka fallible initialization](https://docs.rs/moka/0.12.16/moka/sync/struct.Cache.html#method.try_get_with),
[Quick Cache native guard](https://docs.rs/quick_cache/0.7.0/quick_cache/sync/struct.Cache.html#method.get_value_or_guard),
[Quick Cache guard implementation](https://docs.rs/quick_cache/0.7.0/src/quick_cache/sync_placeholder.rs.html),
and [Lifecycle](https://docs.rs/quick_cache/0.7.0/quick_cache/trait.Lifecycle.html).
