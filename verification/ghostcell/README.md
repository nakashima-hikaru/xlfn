# Branded invocation storage for handle decoding

## Design

Each real UDF invocation creates a fresh `GhostToken` at its existing internal
call boundary. `CallStorage<'brand, 'call>` owns an append-only
`GhostCell<'brand, HandlePermits>` and the existing unbranded `CallScope<'call>`.
The retained permits are declared first, so they are destroyed before scratch.

`DecodeCallContext` owns the token by value in its runtime access variant.
Resolving a handle mutably borrows the context only for the registration step;
the resulting witness and `Handle<'call, T>` borrow the call lifetime, not the
token. Multiple arguments therefore coexist while decoding continues.

Consuming the argument context erases the brand and transfers the existing
resolver into the unbranded return context. It does not destroy `CallStorage`:
permits protect existing handles through the rest of the call, including return
publication. `FinishedArguments`, `ReturnContext`, and `FormulaPublisher` do not
acquire a brand parameter.

The public `FromExcel` and `PrepareExcel` conversion contracts remain unchanged.
The sealed internal `ExcelParameter` conversion path now accepts mutable decode
access, including nested collection and optional conversion. Scratch-only helpers
retain unbranded access with no ability to issue a handle-domain witness.

## Safety obligations

GhostCell enforces aliasing, not retention. Soundness also requires all of:

1. Storage construction stays within generative HRTB closures. The invariant
   call lifetime excludes domains whose owners are local to the operation.
2. A witness is issued only after its domain's permit has been successfully
   retained. No constructor takes an arbitrary scratch-only scope.
3. Permits remain append-only until storage teardown. No external API exposes
   the GhostCell or its inner value for removal.
4. A witness borrows the scope field through the enclosing storage borrow.
   It cannot outlive that owner, even though its type does not mention the brand.
5. The admitted call guard or explicit enclosing state outlives storage and
   every retained permit's maintenance tail.
6. Finishing decoding releases token authority but not the storage. Thread-bound
   handle and storage restrictions remain intact.

There is no new unsafe lifetime coercion. The existing owned-domain entry still
requires an explicit safety argument. The concurrent read-domain protocol and
append-only `Retained` implementation are unchanged.

Some storage/helper types are `#[doc(hidden)] pub` for the generated macro ABI and
external verification fixtures. This is not a claim that the type itself is
private: its construction and fields are private. Hidden `CallFrame` and
`ArgumentContext` types gain a brand lifetime parameter. Macro-generated UDF
call shapes and normal user-facing conversion traits remain unchanged.

## Verification

The standalone model in `probes/ghostcell-call-model` isolates the lifetime
argument. Actual-library tests and negative fixtures are required in addition;
the model alone is not verification of concurrent registry reclamation.

Reproducible commands:

```sh
just ghostcell-model
cargo test --workspace --all-features --lib --locked -- --test-threads=1
cargo test -p xlfn --all-features --test compile --locked
just typestate
cargo clippy --workspace --all-targets --all-features --locked
cargo +nightly-2026-09-02 miri test -p xlfn --no-default-features --features handles --lib --locked -- miri_
MIRIFLAGS="-Zmiri-tree-borrows" cargo +nightly-2026-09-02 miri test -p xlfn --no-default-features --features handles --lib --locked -- miri_ghostcell
cargo bench -p xlfn --features bench-internals --bench handle_call_resolution --locked -- '^handle_call_resolution/handles/(1|2|8|32)$' --warm-up-time 1 --sample-size 50
```

Results and environment limitations are recorded separately alongside this note.
No performance improvement follows merely from replacing the RefCell: handle
resolution also includes token authentication, service resolution and publication
reads. Measurements must use the same compiler, machine and benchmark protocol.
