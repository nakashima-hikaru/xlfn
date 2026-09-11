# Contributing

This repository contains both the xlfn user guide and the implementation of
the framework itself. User-facing testing and release qualification remain in
the [user guide](guide/src/testing.md); this file describes checks for changes
to xlfn itself.

## Local checks

Use the root `Justfile` as the source of truth for repository checks:

```console
just quick
just check
```

`just quick` runs formatting, the panic-boundary audit, workspace Clippy, and the default nextest
profile. `just check` additionally runs the cargo-hack feature powerset,
benchmark compilation, cargo-deny, and the public API compatibility audit.
Use `just test-libtest` when validating same-process libtest behavior, which is
the execution model retained by the Windows artifact job.

Run `just miri-setup` once before `just miri` or `just miri-cache-backends`.
The `Justfile` pins Miri to `nightly-2026-08-22` for both local checks and CI.
Newer Miri rejects the reference passed to the variadic
Linux futex syscall by `parking_lot_core 0.9.12`; this pin is a temporary
compatibility workaround, not a fix for that dependency. Revisit it when
the dependency is fixed, and validate both Miri recipes before advancing it.
The recipes use the committed lockfile to keep dependency resolution stable.

## API compatibility

`just semver` runs `cargo-semver-checks` for the publishable workspace crates
against the published `0.1.0` tag. The CI checkout fetches the full history so
that this baseline is available in pull requests as well as on `main`.

The tool derives compatibility requirements from each crate's actual version.
The `0.1.0` to `0.2.0` transition permits breaking changes, including the typed
cache endpoint redesign; the tool skips compatibility lints for those crates.
Crates that remain on `0.1.x` still receive compatibility checks. Do not force
a workspace-wide `--release-type major`, which would also skip those checks.
After the next release, advance the baseline to its tag so patch releases are
checked within that compatibility line. Intentional breaking changes require
the appropriate version change and documentation. CLI behavior and
procedural-macro diagnostics are separate compatibility contracts and are not
covered by this audit.

Standalone consumers should also be checked when their interfaces change:

```console
cargo check --manifest-path examples/basic-xll/Cargo.toml --locked
cargo check --manifest-path examples/rtd-source/Cargo.toml --locked
cargo check --manifest-path tests/xlfn-e2e-fixture/Cargo.toml --locked
```

## Compile-fail contracts

Procedural macros and marker traits enforce misuse at compile time. Maintain
compile-fail fixtures for:

- invalid context positions and borrowed context types;
- incompatible `thread_safe`, `macro_sheet`, reference, and async combinations;
- unsupported return modes;
- generic, unsafe, extern, or variadic UDFs;
- invalid defaults and argument names;
- handle producer restrictions;
- invalid application error conversions and adapter configuration.

A diagnostic is part of the interface. Assert relevant error text without
overfitting compiler formatting.

## Concurrency and shutdown

Prefer deterministic barriers at lifecycle race points over stress loops alone.
Cover close racing with call entry, async cancellation and completion, task
drop re-entry, handle identity and retirement, RTD subscribe/publish/notify and
`ServerTerminate`, queue-full shutdown, cache clear versus in-flight
initialization, same-key cache recursion, and required adapter reentry rules.

Use Loom or another model checker for small synchronization cores where
practical, and retain ordinary stress tests for integration pressure. The
`formal/` Lean model is an executable abstraction of the shutdown protocol; it
is evidence for the model, not a proof of the entire Rust implementation.

Every boundary that consumes a panic must use `panic_boundary::catch_no_unwind`
or `contain_panic` for a thread join. A caught payload is arbitrary user data;
discarding a raw `catch_unwind` or `JoinHandle::join` result can run a panicking
destructor. The shared policy safely destroys exact standard string payloads
and deliberately retains custom payloads without calling their destructors.
It cannot contain aborting panics, a panicking panic hook, or a double panic
before the unwind reaches the boundary.

Run `just panic-boundaries` after changing panic handling. The independent CI
job checks all Rust source, including imports, test functions, and generated
macro token streams, against an occurrence-specific allowlist. New consuming
calls use the helper; intentional resume-only exceptions require a specific
reason and inventory update. See the [boundary and join audit](tools/panic-boundary-audit.md).

The [2026-09-07 soundness audit](tools/soundness-audit-2026-09-07.md) maps
publication and reclamation obligations to their regression checks and records
the limits of local Miri, model-checking, and Windows validation.
The [follow-up quality review](tools/quality-review-2026-09-07.md) records the
subsequent concurrency, cleanup, conversion, and packaging regressions.

## Windows artifacts and ABI

The Windows artifact job intentionally uses same-process libtest semantics:

```powershell
cargo test --workspace --all-features --target x86_64-pc-windows-msvc --locked -- --test-threads=1
cargo test --workspace --all-features --target i686-pc-windows-msvc --locked -- --test-threads=1
```

Keep `--test-threads=1`: these fixtures share module-wide admission and callback
state. Use nextest for parallel execution in isolated processes.

The SDK-backed ABI probe is a separate boundary check. It must use the pinned
Excel SDK headers and verify the SDK digest and publisher before extraction.
Check sizes, alignment, offsets, architecture-specific types, calling
conventions, ownership, and a live trampoline where possible. A plain probe
build without `sdk-bindgen` is not equivalent evidence.

## Benchmarks

Criterion benchmarks use the same recipes locally and in the benchmark
workflow:

```console
just bench         # Production regression suite (~36 cases tracked by Bencher)
just bench-ci      # Canonical CI suite (aliased by `just bench` and `just bench-pr`)
just bench-scaling # Multi-threaded scaling and data size expansion curves
just bench-full    # Complete unfiltered suite (~175 cases)
just bench-check   # Clippy lint check across all benchmarks
```

Bencher is the long-term history store and the benchmark workflow maintains a
10% relative latency threshold on the main branch. PR runs clone those
thresholds from the base branch and fail on an alert. To publish, configure the repository
Actions secret `BENCHER_API_KEY` and repository variable `BENCHER_PROJECT`.
Fork pull requests run the benchmark command without publishing because their
secrets are unavailable. Criterion baselines are for local A/B comparisons;
do not commit generated `estimates.json` files.

The threshold is intentionally broad until main-branch noise is characterized;
narrow it only after reviewing several stable runs. The built-in Criterion
latency measure is shared by the benchmark groups, so the threshold is a
repository-wide first guard rather than a per-family policy.

## CI command contract

CI uses separate Windows jobs for Clippy, feature checks, nextest, and the
handle close-race model. Their matrix disables fail-fast so each check reports
its own result. Benchmark lint and Criterion measurements also run in
independent jobs; a lint failure must not suppress test or performance evidence.
The Linux jobs cover portable tooling and Miri independently. Keep the root
`Justfile` recipes authoritative so local and CI flags do not drift.

The Windows artifact job remains on libtest because it validates the
same-process execution model and emits the RTD shutdown trace. The nextest
`windows-rtd` group therefore applies to nextest runs only; RTD tests also take
their process-global test lock.

When changing the guide, run the same mdBook build and validation used by CI.
When changing generated Windows bindings, run the generator and verify that
the tracked generated files have no diff.

## Release evidence

Keep these claims separate:

- **implemented** — source exists;
- **unit-tested** — host tests passed;
- **Windows artifact-tested** — linked x86/x64 package inspection passed;
- **Excel validated** — named real-Excel environments passed;
- **signed/deployed** — final binaries passed organizational release controls.

Do not infer one status from another. Record the supported environment matrix
and known unqualified combinations with release notes.
