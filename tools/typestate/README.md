# Private lifecycle compile checks

From the repository root, run
`uv run --project tools --locked python -B -m xlfn_tools.check_typestate`.
The checker copies the current tracked and nonignored working files into a
temporary workspace, injects fixtures as private internal modules, and runs
offline, locked `cargo check` against each owning package. Facade fixtures
check `xlfn` with `async,rtd,handles,cache`; `kernel/*` fixtures check
`xlfn-kernel` without facade features. The default target is the rustc host.
Windows COM fixtures run on Windows and are explicitly reported as skipped on
other hosts. `--target x86_64-pc-windows-msvc` includes them in a cross-target
check when the Windows native dependency toolchain is configured;
`--extra-features` can enable additional dependency features such as
`blake3/pure` for compile-only checks. CI runs the suite on both Linux and
Windows. These are compile checks, not live Excel execution tests. The original
source tree is never edited.

Positive callers run first, batched by package, features and compile target. A
failed batch is checked again fixture by fixture to localize the failure; any
failed positive check withholds all negative evidence. Negative callers are
always compiled one at a time. Use `--jobs 2` to run two negative workers with
separate temporary workspaces and Cargo target directories (the default is one
worker to limit local disk use). Reports and logs remain in fixture order.
Negative callers must produce the exact error code at each line marked
`// error: E....`; unrelated errors fail the check.

## Scope

Fixtures here are intentionally limited to non-trivial compile-time soundness
and contract invariants that are easy to break silently during refactoring and
cannot be verified from outside the crate with `trybuild`:

- **Lifetime, borrow-escape, and drop-order bounds** (`E0499`, `E0502`,
  `E0505`, `E0507`, `E0515`, `E0521`, `E0597`): guards, leases, GhostCell
  domains, quota/handle capabilities, and COM references cannot outlive or drop
  before their owning scope.
- **Subtyping and variance invariance** (`E0308` on `for<'a> fn(&'a _)` vs
  `fn(&'static _)`): typed cache endpoints/pointers, handle aliases/payloads,
  input materializers, thread-affine slots/transactions, and COM termination
  work remain invariant in their payload types.
- **Auto-trait and sealed-trait bounds** (`E0277`): `!Send`/`!Sync` guards,
  `Send` cache requirements, sealed unload/retirement traits, and audited
  subscription/publication tokens.
- **`unsafe` boundary enforcement** (`E0133`): detached permits and raw
  published-owner dereferences remain `unsafe`.

Paired `pass/` fixtures act as positive controls for the kept modules. Trivial
checks of ordinary Rust rules (repeated moves `E0382`, missing methods `E0599`,
private field/method visibility `E0616`/`E0451`/`E0624`/`E0603`, wrong arity
`E0061`, and plain concrete type mismatches) are intentionally excluded.
