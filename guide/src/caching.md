# Calculation caches

Use a calculation cache to reuse an expensive computation across calls.
Enable `cache`. Keys must identify the full computation, and cached values
must be owned `Send + Sync + 'static` Rust data. A cache does not give a
worksheet formula ownership of an object; see [Choosing a pattern](choosing-pattern.md).

Import from:

```rust
use xlfn::cache::{
    CacheEndpoint, CacheLease, CacheRegistry, CalculationCache, CanonicalF64,
};
```

## One typed cache

`CalculationCache<K, V>` is a concurrent weighted cache with a bounded resident budget and one weight policy fixed at construction:

```rust
# use xlfn::cache::CalculationCache;
# fn example() -> xlfn::XllResult<()> {
let cache = CalculationCache::<String, String>::with_weigher(1024, String::len);
let value = cache.get_or_try_insert_with(
    "abc".to_owned(),
    || Ok("ABC".to_owned()),
)?;
assert_eq!(&*value, "ABC");
# Ok(())
# }
# example().unwrap();
```

This small example uses string length as weight. In an application, replace
the compute closure with an expensive deterministic calculation. The `String::len` weigher belongs to the cache and cannot vary between callers.

The returned value is `CacheLease<'_, V>`, which implements `Deref<Target = V>`. Concurrent initializations for the same key are coalesced. A failed initialization is returned to its caller and is not cached.

The weight budget is an abstract integer. It can represent approximate bytes, external-resource units, or another monotone cost, and the cache applies its construction-time policy to every insertion. `new(budget)` uses unit weights. Zero is normalized to a minimum positive cache weight. A value heavier than the entire budget is returned but not retained.

`len()` and `used_weight()` report current residency. A live `CacheLease` keeps
its value readable even after eviction or `clear()`.

Each node uses a portable `u32` pin count, including its resident, initialization
and lease pins, on both 32-bit and 64-bit hosts. The existing invariant policy
stops the process if another pin would exceed `u32::MAX`; the count never wraps
or resurrects a node after its final pin is released. Cache budgets, weights,
collection indices and admission-domain counters retain their existing types.

## Typed endpoint registry

`CacheRegistry` creates caches lazily for static endpoints:

```rust
# use xlfn::prelude::*;
# use xlfn::cache::{CacheEndpoint, CacheLease, CacheRegistry};
static UPPERCASE: CacheEndpoint<String, String> = CacheEndpoint::new("uppercase-v1").with_weigher(String::len);

struct State {
    caches: CacheRegistry,
}

fn build_state() -> State {
    State {
        caches: CacheRegistry::new(1024),
    }
}

fn uppercase<'a>(state: &'a State, text: String) -> XllResult<CacheLease<'a, String>> {
    state.caches.get_or_try_insert(
        &UPPERCASE,
        text.clone(),
        || Ok(text.to_uppercase()),
    )
}
```

`CacheEndpoint<K, V, Marker = ()>` is a reusable descriptor. It borrows no
registry, so it can be a `static` while each add-in state owns a `CacheRegistry`.

An endpoint identity includes its marker type, key type, value type, and static ID. By default, `Marker = ()`. When multiple endpoints share key and value types, an optional marker type (e.g. `CacheEndpoint<DatasetKey, Dataset, LookupMarker>`) provides semantic disambiguation.

Declare a custom weigher once on the shared endpoint. A conflicting policy for the same endpoint identity returns an error, including on warm lookup paths; use another marker or ID for a different budget measure. Reuse the same descriptor rather than independently constructing descriptors with function pointers.

A registry keeps its endpoint caches for its own lifetime. Clearing values
does not change endpoint identity. Endpoints in different registries do not
share cached values.

Use versioned IDs when a cached value's meaning changes:

```rust
# use xlfn::cache::CacheEndpoint;
static UPPERCASE: CacheEndpoint<String, String> = CacheEndpoint::new("uppercase-v2");
```

Changing an algorithm without changing the endpoint or key can silently reuse a value produced under old semantics in a long-lived Excel process.

## Float keys

Do not use raw `f64` as an ordinary hash key. `CanonicalF64` rejects NaN and infinity and normalizes signed zero:

```rust
# use xlfn::cache::CanonicalF64;
# fn example(x: f64, y: f64) -> xlfn::XllResult<()> {
#[derive(Clone, Eq, Hash, PartialEq)]
struct QueryKey {
    x: CanonicalF64,
    y: CanonicalF64,
}

let key = QueryKey {
    x: CanonicalF64::new(x)?,
    y: CanonicalF64::new(y)?,
};
# Ok(())
# }
```

This solves basic finite-value hashing; it does not define a tolerance. When approximate equality is a domain requirement, quantize explicitly and document the error bound.

## Clearing and generations

`clear()` advances a generation and invalidates older entries. In-flight computations that began before the clear may finish and return to their caller, but they cannot repopulate the new generation with stale results.

```rust
{{#include ../fixtures/cache.md}}
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

Keys must keep equality and hashing stable while stored. `Hash`, `Eq`, and
`Clone` must not start another cache initialization; recursive access can
deadlock.

The compute and weight functions execute application code. They must:

- avoid panics;
- avoid unbounded blocking while internal single-flight state is held;
- return owned `Send + Sync + 'static` values;
- avoid callbacks into Excel;
- use a deterministic key-to-value contract.

For the difference between a cache and a worksheet-owned object, see
[Choose a calculation pattern](choosing-pattern.md).
