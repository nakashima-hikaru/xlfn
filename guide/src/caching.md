# Calculation caches

The advanced cache module provides concurrent, bounded memoization for application data. It is independent of formula-owned handles: a handle controls worksheet ownership, while a cache controls reuse of an internal computation.

Import from:

```rust
use xlfn::advanced::cache::{
    BoundCacheEndpoint, CacheEndpoint, CacheRegistry, CalculationCache, CanonicalF64,
};
```

## One typed cache

`CalculationCache<K, V>` uses Moka's TinyLFU admission/eviction policy and a caller-defined weight:

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

The returned value is `Arc<V>`. Concurrent initializations for the same key are coalesced. A failed initialization is returned to its caller and is not cached.

The weight budget is an abstract integer. It can represent approximate bytes, external-resource units, or another monotone cost, but every call site for a cache must use one consistent definition. Zero is normalized to a minimum positive cache weight. A value heavier than the entire budget is returned but not retained.

Metrics such as `len()` and `used_weight()` run pending Moka maintenance first, but should still be treated as operational estimates rather than transactional accounting.

## Typed endpoint registry

`CacheRegistry` creates caches lazily for static endpoints:

```rust
enum LookupEndpoint {}

static LOOKUP_DATASETS: CacheEndpoint<
    LookupEndpoint,
    DatasetKey,
    Dataset,
> = CacheEndpoint::new("lookup-datasets-v1");

struct State {
    datasets: BoundCacheEndpoint<LookupEndpoint, DatasetKey, Dataset>,
}

fn build_state(caches: &CacheRegistry) -> XllResult<State> {
    Ok(State {
        datasets: caches.bind(&LOOKUP_DATASETS)?,
    })
}

fn cached_dataset(state: &State, key: DatasetKey) -> XllResult<Arc<Dataset>> {
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

Recursive initialization of the same cache key on the same thread is rejected rather than allowed to deadlock. A computation function should therefore not request the same key from the same cache. Decompose dependencies into separate endpoints or compute the lower layer directly.

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
