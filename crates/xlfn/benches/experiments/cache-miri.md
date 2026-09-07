# Full-cache Miri qualification

The cache regression tests and the kernel's Miri tests are separate evidence.
Do not describe the complete Moka-backed cache as Miri-clean with the current
dependency/toolchain combination.

On 2026-09-07, using `rustc 1.100.0-nightly (c656540d6 2026-08-21)` on
`aarch64-apple-darwin`, the following extra diagnostic was attempted:

```sh
CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" \
MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test \
  -p xlfn --features "unstable-cache bench-internals" --lib \
  cache::tests::scoped_reference_survives_eviction_until_scope_drop --locked
```

Stacked Borrows reports an invalid retag in
`crossbeam-epoch 0.9.20/src/internal.rs`, in `Local::element_of`. The observed
pointer tag covers the intrusive list entry rather than the entire `Local`.
This occurs in Moka's first lookup, before the xlfn node is initialized.

An isolated crate with only `moka = { version = "=0.12.16",
default-features = false, features = ["sync"] }` and the following program
reproduced the same retag failure without any xlfn dependency. That isolated
resolution selected `crossbeam-epoch 0.9.21`, so updating just to that patch
version did not remove the diagnostic.

```rust
fn main() {
    let cache = moka::sync::Cache::<u32, u32>::new(8);
    for key in 0..16 {
        let _ = cache.get(&key);
        cache.insert(key, key);
    }
    cache.invalidate_all();
    cache.run_pending_tasks();
}
```

As a diagnostic comparison, adding `-Zmiri-tree-borrows` let the xlfn scoped
eviction test finish successfully. Miri then reported five leaked allocations
in Crossbeam's `Local`/`SealedBag` collector structures at process exit. None of
those five allocation reports originated from `CacheNode` or its payload.
The run still exited unsuccessfully; leak checking was not disabled.

These experimental borrow models differ. The isolated reproduction narrows
this blocker to the dependency path; it neither proves all cache operations
safe nor establishes a production-use-after-free in xlfn. Keep this diagnostic
separate from the kernel's passing Miri tests and from the native DropProbe,
concurrency, and retirement-debt regressions. Revisit full-cache qualification
when the dependency/toolchain combination can run it without these reports.

## Sharded resident index baseline (2026-09-07)

The initial `just miri-cache-sharded` baseline ran nine backend-common full-cache
tests against all four shard counts (8/16/32/64). Both Stacked Borrows (84.37 s) and Tree
Borrows (72.86 s) passed on the nightly above, with leak checking and alias
validation enabled and without disabling isolation. This includes live leases,
scoped-reference eviction, concurrent clear, generation rollover, duplicate
initializers, reentrancy, failed/panicking initialization, weighted bounds,
exactly-once destruction, concurrent reads/writes, and retirement-debt drain.
Miri uses reduced operation/thread counts, while executing the actual cache.

The [follow-up quality review](../../../../tools/quality-review-2026-09-07.md)
added two backend-common regressions: draining retired values after an
initializer panics, and deferring that cleanup while a reader is active.
Both also passed separately under both borrow models with leak and alias
validation enabled; the recipe now includes all eleven tests on future runs.

`parking_lot_core 0.9.12` emitted an integer-to-pointer provenance warning in
its contended lock path. Neither run reported an alias violation or a leak;
the passing runs are not strict-provenance qualification of parking_lot.

The Moka production configuration remains **unqualified for full-cache Miri**
because of the dependency blocker described above. Only rerun that Moka
qualification when Moka, crossbeam-epoch, Miri, or nightly changes. The native
common regressions and the sharded Miri baseline remain separate evidence; do
not exclude the failing dependency path and call the production cache Miri-clean.
