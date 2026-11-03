# Validated thread-affine binding claims

A `ThreadAffineAccess` is a non-Send, non-Sync witness tied to its issuing
slot and a binding generation. It can outlive release and multiple witnesses
can refer to one generation. Therefore `with_mut`, `has_value`, and `binding`
continue checking generation, bound state, and current owner each time.
The slot-bound wrappers additionally reject a witness issued by a different
slot. Runtime methods accept a separate runtime and access and must keep this
issuer check; calling `access.binding()` there would be insufficient.

`binding()` validates the witness and sets `modifying` in the same mutex
critical section. It then constructs a private `BindingClaim` **before** the
TLS occupancy probe. Consequently TLS unavailability, a reentrant TLS borrow,
or a panic drops the claim and clears the modification flag. No mutex is held
through a user callback.

The claim pins the checked generation and exclusive occupancy transition
rights. Sibling access witnesses can read or mutate the existing payload, but
cannot acquire another claim to install, remove or release it. `BindingTxn`
encodes Empty/Occupied phases; its private claim methods carry out TLS work
without revalidating the pinned owner or generation. TLS availability,
reentrancy, occupied/missing state, and downcast errors remain checked. On
failure, `BindingInstallError` returns the original value and empty transaction;
take/release failure returns the original transaction.

Empty release checks the TLS entry, then clears the owner under the mutex.
`modifying` stays true until the claim drops. During the interval between those
steps, a different thread cannot rebind. Forgetting a transaction leaves the
claim permanently set and preserves the payload; it does not turn a leaked
claim into permission for another transition. These are temporal invariants of
the claim, not consequences of ordinary access lifetime alone.

## Validation

`cargo test -p xlfn-kernel --lib thread_affine --locked` covers 16 tests,
including stale and cross-slot access, nested TLS borrowing, value and
transaction recovery, rebind during release, and a forgotten occupied claim.
An additional xlfn runtime test checks foreign issuer rejection through both
runtime receivers and all existing operations. The eight `miri_` binding
cases pass on nightly-2026-09-02 under both Stacked Borrows and Tree Borrows;
the foreign runtime case also passes Miri. Running **all** thread-affine tests
under Miri without a filter reports leaks from the existing intentional TLS
teardown leak test. That case is designed to leak when teardown cannot prove a
safe unload; the filtered Miri runs retain leak checking.

`tools/typestate` continues to assert that low-level claimed operations cannot
be called through a slot without the private claim. Full-workspace test and CI
status are tracked in the PR rather than inferred from the focused tests.

## Restricted microbenchmark

The example `crates/xlfn-kernel/examples/binding_claim_micro.rs` runs a
single-thread loop of bind, acquire Empty claim, install a `u64`, take, and
release. It includes TLS lookup, allocation, and generation rebinding. Run via
`cargo run --release -p xlfn-kernel --example binding_claim_micro --locked`.
The same source was compiled once against the base and once against the patch;
six processes were alternated, 500,000 iterations each, using Rust 1.99.1 on
an x86_64 AMD EPYC 9V74 cloud host. Results in ns per cycle:

| Round | Base | Claim | 
| --- | ---: | ---: |
| 1 | 74.339 | 48.482 |
| 2 | 73.398 | 39.533 |
| 3 | 81.693 | 52.895 |
| 4 | 69.574 | 41.516 |
| 5 | 73.493 | 38.529 |
| 6 | 80.689 | 40.131 |

The median fell from 73.916 to 40.824 ns in this narrowly scoped, unpinned
local experiment. This is not a statistical guarantee, an Excel benchmark, or
a claim of comparable end-to-end improvement. No CI threshold is changed.
