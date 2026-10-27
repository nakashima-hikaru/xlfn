# Private lifecycle compile checks

Run `python3 tools/check_typestate.py` from any directory. The checker copies the
current tracked and nonignored working files into a temporary workspace, injects
each fixture as an internal module, and runs an offline, locked `cargo check`
with `async,rtd,handles,cache`. The original source tree is never edited. An optional
`--report PATH` retains toolchain, input hash, and diagnostics.

Positive callers run first. Negative callers must produce the exact error code
at each line marked `// error: E....`; unrelated errors fail the check. Internal
injection verifies the actual method and field restrictions without publishing
test APIs or accepting an ancestor-module privacy error as typestate evidence.

Server termination fixtures also require cancellation before finish, reject
reusing cancellation or completion authority, and prevent passing a cancellation
result from another termination. Coordinator fixtures check that only its affine
owner can publish one terminal result and that shared state remains private.

Typed handle fixtures also reject payload retagging and subtype conversions
that would change the exact `TypeId`. Input fixtures check static materializer
selection and consuming prepared values once. Teardown fixtures require recovery
authority while permitting borrowed stages, and cache fixtures enforce its
thread-safety bounds.

Endpoint memoization fixtures require the endpoint and retained cache pointer
to have the same key, value, and marker types, including function subtyping.
Unload-policy fixtures allow both policies for an opted-in add-in, require the
physical-unload contract for the physical policy, and reject policy retagging
or unchecked implementations of the sealed policy trait.

Source subscription tickets can transfer across threads but cannot be shared
concurrently or consumed twice. Their source-local slot has one cancellation
and disconnection owner even when the concrete subscription is not `Sync`.
