# Scoped intrusive lifecycle reentry stack

Each host lifecycle invocation registers one pinned local `LifecycleFrame` in a
thread-local pointer chain. Traversal rejects any runtime already present, so
A → B → C → A is rejected without hiding the outer runtime. Registration is
private to `with_host_lifecycle`: the caller receives a borrow, never the guard.
The `HostCloseBoundary<'scope>` borrow is issued only by the close path and is
validated against the receiving runtime before residency release. Open/remove do
not gain close authority. Existing opening-deactivation intent and phase handling
remain outside the reentry mechanism.

## Safety obligations

The only pointer dereference is chain traversal. Every published node is pinned
on its invocation's stack and is removed before that invocation returns or unwinds.
The previous pointer was a live outer frame at insertion. Each private guard
checks that its own frame is the current head and aborts on a violated LIFO
invariant rather than following a corrupted chain. No user callback or fallible
operation occurs between pointer publication and guard construction. Because
callers cannot access the guard, forgetting the borrowed close capability does
not forget the guard. Ordinary Rust unwinding runs the guard and restores TLS.

This requires a host that does not perform a foreign non-local jump over a Rust
frame. Such a jump would bypass `Drop`, leaving a dangling stack pointer in TLS;
the previous vector-based implementation stored identities without dereferencing
those identities. Rust's normal unwind is supported, but longjmp-like behavior is
not covered by these tests. The TLS cell has no destructor. Neither TLS identity
nor the frame grants cross-thread authority.

## Validation and comparison

The standalone `probes/lifecycle-stack-model` uses a standard-library-only core
and separate dev-only SmallVec comparison. It tests nesting, deep cycles, panic
restoration, `mem::forget` of the borrowed capability, thread isolation, and
compile-fail escape/thread transfer. Use `cargo test --manifest-path
probes/lifecycle-stack-model/Cargo.toml --locked` and the same command prefixed
`cargo +nightly-2026-09-02 miri test`; repeat with
`MIRIFLAGS=-Zmiri-tree-borrows`. Actual library tests and typestate fixtures are
also necessary: this bounded model does not prove the surrounding host protocol.

A five-process, unpinned Rust 1.99.1 synthetic comparison on x86_64 compared
nesting depths 1, 2, and 8 against `ManuallyDrop<RefCell<SmallVec<[usize; 1]>>>`
that explicitly frees spilled storage on the final pop. Both allocated zero at
depth 1. The intrusive version allocated zero at depths 2 and 8; the SmallVec
comparison incurred 1 and 3 allocation/reallocation events respectively. TLS
payload sizes were 8 versus 32 bytes. Depth-1 elapsed ranges were 5.63–6.27 ns intrusive and 5.29–10.09 ns
SmallVec. Depth 2 was 8.0–10.2 ns versus 23.38–36.83 ns; depth 8 was
33.52–45.11 ns versus 110.37–172.90 ns. These are nanoseconds per nested
entry/exit batch, not per single node; allocation counts include reallocations. This is not an
Excel timing, and a SmallVec design retaining spilled capacity is a different
tradeoff. Unsafe complexity and host stack discipline, not this narrow timing,
are essential to the adoption decision.


At this checkpoint, actual `miri_intrusive_` library tests pass under both
Stacked Borrows and Tree Borrows, including deep reentry, panic restoration,
forgotten capability and thread isolation. The scoped HostCloseBoundary positive
fixture and its E0521 escape-negative fixture pass. Five residency tests and
the all-feature production check pass. Full workspace all-feature library
tests: 1,657 pass, 7 ignored; workspace all-target all-feature Clippy passes.
The panic inventory includes only two explicit test-specific unwind entries.
These are local results, not hosted CI or a real-Excel qualification.
