# Calculation caches

The experimental cache module provides concurrent, bounded memoization for application data. It is independent of formula-owned handles: a handle controls worksheet ownership, while a cache controls reuse of an internal computation. Enable the explicit `unstable-cache` crate feature to use it.

Import from:

```rust
use xlfn::unstable::cache::{
    BoundCacheEndpoint, CacheEndpoint, CacheLease, CacheRegistry, CalculationCache, CanonicalF64,
};
```

## One typed cache

`CalculationCache<K, V>` uses Quick Cache's weighted admission/eviction policy with one native shard and a caller-defined weight:

```rust
#[derive(Clone, Eq, Hash, PartialEq)]
struct DatasetKey {
    currency: String,
    as_of: i32,
}

let cache = CalculationCache::<DatasetKey, Dataset>::new(64 * 1024 * 1024);

let dataset = cache.get_or_try_insert_with(
    key.clone(),
    |dataset| dataset.estimated_bytes(),
    || build_dataset(&key),
)?;
```

The returned value is `CacheLease<'_, V>`, which implements `Deref<Target = V>`. Concurrent initializations for the same versioned key share a short-lived flight owned by xlfn. All callers waiting on that flight receive the same initialization error; a later call can retry. If the initializer panics, waiting callers can retry leadership. Initializers run outside the flight and resident-index locks. Completed flights are immediately removed and hold no cache nodes.

The weight budget is an abstract integer. It can represent approximate bytes, external-resource units, or another monotone cost, but every call site for a cache must use one consistent definition. Zero entry weight is normalized to one. A zero cache budget bypasses residency. A value heavier than the entire budget is returned but not retained. Native admission may also reject smaller values: the budget is an upper bound, not a promise that a particular value remains cached. One shard preserves the exact global resident-weight limit without rounding per-shard quotas upward.

`len()` and `used_weight()` observe native resident counts and opportunistically reclaim already-retired nodes. Individual snapshots are bounded, but concurrent mutations mean separate metric reads are not a transaction. Caller weights do not include index overhead, active flights or retained leases.

Eviction and memory reclamation are separate. A live lease intentionally keeps
its value alive after eviction or `clear()`. Once the final pin is released,
the value enters a retirement queue until readers that could have observed its
pointer have finished. Native eviction only collects entries in a lifecycle
request state. After the native lock is released, that state invokes xlfn's
existing retirement callback; it never runs value destructors under the lock.

Ordinary reads attempt reclamation when work is queued, without waiting for
readers. Initialization attempts apply backpressure when queued retirement reaches 256 nodes or the
endpoint's weight budget: the operation waits for existing readers before
returning. This bounds accumulating debt during ongoing mutation, subject to
concurrent operations; it is not a strict bound on process memory. A final lease
drop and explicit clear also wait for reclamation. There is no background
reclamation thread; idle caches can retain their last small batch until another
operation or destruction.

`reclamation_stats()` on a cache or bound endpoint returns approximate counters
without performing maintenance: pending nodes and weight, their peak values,
the number of nodes handed to reclamation, the largest batch, and cumulative
grace-period time. Pending counts exclude resident entries and values retained
by live leases. Use these counters with `used_weight()` to distinguish eviction
capacity from retirement debt; caller-defined weights do not measure allocator
overhead or the process's actual memory usage.

## Typed endpoint registry

`CacheRegistry` creates caches lazily for static endpoints:

```rust
enum LookupEndpoint {}

static LOOKUP_DATASETS: CacheEndpoint<
    LookupEndpoint,
    DatasetKey,
    Dataset,
> = CacheEndpoint::new("lookup-datasets-v1");

struct State<'registry> {
    datasets: BoundCacheEndpoint<'registry, LookupEndpoint, DatasetKey, Dataset>,
}

fn build_state<'registry>(caches: &'registry CacheRegistry) -> XllResult<State<'registry>> {
    Ok(State {
        datasets: caches.bind(&LOOKUP_DATASETS)?,
    })
}

fn cached_dataset<'a>(state: &'a State<'_>, key: DatasetKey) -> XllResult<CacheLease<'a, Dataset>> {
    state.datasets.get_or_try_insert(
        key.clone(),
        |dataset| dataset.estimated_bytes(),
        || build_dataset(&key),
    )
}
```

An endpoint identity includes its marker type, key type, value type, and static ID. The marker gives semantically different caches separate identities even when key and value types are the same.

Use versioned IDs when a cached value's meaning changes:

```rust
CacheEndpoint::new("lookup-datasets-v1")
```

Changing an algorithm without changing the endpoint or key can silently reuse a value produced under old semantics in a long-lived Excel process.

## Float keys

Do not use raw `f64` as an ordinary hash key. `CanonicalF64` rejects NaN and infinity and normalizes signed zero:

```rust
#[derive(Clone, Eq, Hash, PartialEq)]
struct QueryKey {
    alpha: CanonicalF64,
    beta: CanonicalF64,
}

let key = QueryKey {
    alpha: CanonicalF64::new(x)?,
    beta: CanonicalF64::new(y)?,
};
```

This solves basic finite-value hashing; it does not define a tolerance. When approximate equality is a domain requirement, quantize explicitly and document the error bound.

## Clearing and generations

`clear()` advances a generation and invalidates older entries. In-flight computations that began before the clear may finish and return to their caller, but they cannot repopulate the new generation with stale results.

```rust
state.caches.clear();
```

The framework does not know when application external data, configuration, or adapter state has changed. The add-in owns invalidation policy. Common triggers include:

- an explicit worksheet/admin refresh function;
- a new external-data snapshot ID;
- configuration reload;
- a calculation-generation boundary when the cache is truly calculation-scoped;
- add-in close.

Prefer putting immutable dependency versions in the key. Broad clears are useful as a safety mechanism, but versioned keys give more precise reproducibility.

## Reentry and computation rules

Starting another cache initialization on the same thread from a compute or
weight function is rejected, including a different key or endpoint. Reading
already-cached values is supported. Compute lower layers directly or resolve
their cache dependencies before entering the initializer.

The compute and weight functions execute application code. They must:

- avoid panics;
- avoid unbounded blocking while internal single-flight state is held;
- return owned `Send + Sync + 'static` values;
- avoid callbacks into Excel;
- use a deterministic key-to-value contract.

Panic containment prevents a permanently stuck initializer, but a panic still indicates a defect.

## Cache versus handle versus RTD

| Need | Facility |
|---|---|
| reuse an internal pure or versioned computation | cache |
| let one worksheet formula own a typed object | handle |
| update a formula repeatedly from a push source | RTD |

They may be composed. For example, a handle producer can obtain immutable calibrated data from a cache, then create a formula-owned lightweight view over it.
