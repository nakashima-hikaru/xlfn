# Calculation caches

The cache module provides concurrent, bounded memoization for application data. It is independent of formula-owned handles: a handle controls worksheet ownership, while a cache controls reuse of an internal computation. Enable the `cache` crate feature to use it.

Import from:

```rust
use xlfn::cache::{
    CacheEndpoint, CacheLease, CacheRegistry, CalculationCache, CanonicalF64,
};
```

## One typed cache

`CalculationCache<K, V>` is a concurrent weighted cache with a bounded resident budget and caller-defined entry weights:

```rust
#[derive(Clone, Eq, Hash, PartialEq)]
struct DatasetKey {
    namespace: String,
    version: i32,
}

let cache = CalculationCache::<DatasetKey, Dataset>::new(64 * 1024 * 1024);

let dataset = cache.get_or_try_insert_with(
    key.clone(),
    |dataset| dataset.estimated_bytes(),
    || build_dataset(&key),
)?;
```

The returned value is `CacheLease<'_, V>`, which implements `Deref<Target = V>`. Concurrent initializations for the same key are coalesced. A failed initialization is returned to its caller and is not cached.

The weight budget is an abstract integer. It can represent approximate bytes, external-resource units, or another monotone cost, but every call site for a cache must use one consistent definition. Zero is normalized to a minimum positive cache weight. A value heavier than the entire budget is returned but not retained.

Metrics such as `len()` and `used_weight()` observe current residency. Concurrent changes mean these remain operational estimates rather than a transactional snapshot.

Eviction and memory reclamation are separate. A live lease intentionally keeps its value alive after eviction or `clear()`. The implementation may apply synchronous maintenance or backpressure to prevent unbounded retirement debt during continuous mutation. Idle caches can retain retired entries until a subsequent cache operation or destruction.

## Guaranteed observable semantics

The stable cache contract guarantees:

- **Same-key single flight**: Concurrent initializations for the same key coalesce so that the compute closure runs once, returning valid leases to all concurrent callers.
- **Failed computations**: If an initializer returns an error, nothing is published into the cache, allowing future calls to re-attempt computation.
- **Clear generation semantics**: Calling `clear()` advances the cache epoch and invalidates existing entries. An in-flight computation that began before `clear()` still returns its value to its immediate caller, but does not repopulate the new generation with a stale result.
- **Lease stability**: A `CacheLease` remains valid and readable for its full lifetime, even if the underlying entry is evicted or `clear()` is invoked.
- **Weight normalization**: A weight of 0 is normalized to the minimum positive cache weight. Overweight values exceeding the budget are returned to the caller but are not retained in resident storage.
- **Reentrancy**: Reentrant cache initialization from within a compute or weight callback on the same thread is unsupported.

## Typed endpoint registry

`CacheRegistry` creates caches lazily for static endpoints:

```rust
static LOOKUP_DATASETS: CacheEndpoint<DatasetKey, Dataset> =
    CacheEndpoint::new("lookup-datasets-v1");

struct State {
    caches: CacheRegistry,
}

fn build_state() -> State {
    State {
        caches: CacheRegistry::new(),
    }
}

fn cached_dataset<'a>(state: &'a State, key: DatasetKey) -> XllResult<CacheLease<'a, Dataset>> {
    state.caches.get_or_try_insert(
        &LOOKUP_DATASETS,
        key.clone(),
        |dataset| dataset.estimated_bytes(),
        || build_dataset(&key),
    )
}
```

You can also perform operations directly through the endpoint descriptor:

```rust
fn cached_dataset<'a>(state: &'a State, key: DatasetKey) -> XllResult<CacheLease<'a, Dataset>> {
    LOOKUP_DATASETS.get_or_try_insert(
        &state.caches,
        key.clone(),
        |dataset| dataset.estimated_bytes(),
        || build_dataset(&key),
    )
}
```

`CacheEndpoint<K, V, Marker = ()>` is a `'static` descriptor that holds no references to `CacheRegistry`, completely avoiding self-referential lifetimes in `SharedState`.

An endpoint identity includes its marker type, key type, value type, and static ID. By default, `Marker = ()`. When multiple endpoints share key and value types, an optional marker type (e.g. `CacheEndpoint<DatasetKey, Dataset, LookupMarker>`) provides semantic disambiguation.

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
    x: CanonicalF64,
    y: CanonicalF64,
}

let key = QueryKey {
    x: CanonicalF64::new(x)?,
    y: CanonicalF64::new(y)?,
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
