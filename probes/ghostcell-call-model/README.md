# GhostCell call-lifetime model

This standalone crate isolates the ownership structure of the call-scope
redesign. It is not an alternative implementation or a proof of the registry's
concurrent reclamation protocol.

## Safety invariants

Each invocation creates a fresh `GhostToken` brand. The independent, invariant
`'call` lifetime covers input storage and handles. The decoding context owns its
token, but only borrows it mutably during registration: returned witnesses and
handles borrow call storage, not the context or token.

`CallStorage` owns the permit collection and the scratch scope. Both fields and
its constructor are private. A witness is constructed only after its domain's
permit is retained. Registration is append-only, and requiring the domain for
invariant `'call` rejects domain owners created inside the invocation closure.
GhostCell establishes aliasing discipline; these separate constraints establish
permit survival and owner lifetime.

Finishing decoding consumes the token and returns an unbranded context without
destroying storage. Existing handles remain usable. Permits precede scratch in
field declaration order and are therefore released before scratch on normal
exit and unwinding.

The model uses a vector and single-threaded reader counters. The actual library
retains its existing allocation-free first-permit representation, domain protocol,
and thread-bound handle access. Actual-library integration tests remain necessary.

## Reproduce

From the repository root:

```sh
cargo test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked
cargo +nightly-2026-09-02 miri test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked
MIRIFLAGS="-Zmiri-tree-borrows" cargo +nightly-2026-09-02 miri test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked
```

If the home directory is read-only, set `XDG_CACHE_HOME` to a writable directory
for Miri's sysroot cache.

Five unit tests cover simultaneous handles across decoding finalization,
repeated-domain deduplication, multiple-domain append-only growth, panic teardown,
fresh invocation registration, and token/cell layouts. Three compile-fail examples
reject handle escape, operation-local domain registration, and mixed fresh brands.
