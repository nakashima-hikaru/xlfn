# Validation record (2026-11-03)

Base: `729a1ed4bb412dbef51047c678c5e59af7a522c9`. Linux x86_64, Rust 1.99.1.
The records below were obtained after restoring the implementation; results from
the earlier lost executor snapshot are not used as proof of this source.

## Passed locally

- Workspace all-feature library tests: 1,640 passed, 7 ignored.
- Core-only xlfn library tests: 599 passed, 6 ignored.
- Workspace all-target/all-feature Clippy with repository warning policy.
- All-feature external positive/negative UI suite, including reviewed snapshots.
- Private typestate fixtures: 468 passed; 48 Windows-only fixtures skipped on Linux.
- Actual xlfn handle-feature Miri suite: 162 passed, pinned nightly-2026-09-02.
- Three focused actual GhostCell tests under Tree Borrows.
- Standalone model: 5 unit tests and 3 compile-fail doctests, ordinary Rust and
  Miri under both Stacked Borrows and Tree Borrows. Model Clippy also passes.
- Rust formatting and workspace documentation with warnings denied.
- Python Ruff, format, ty, and 75 tooling unit tests.
- Strict panic inventory: 55 reviewed references, including the model's explicitly
  scoped unwind test; 5 inventory guard tests pass. No path-wide exemption added.
- RTD consumer feature matrix: 20 positive/negative cases.
- Excel comparison host tests: 13 passed.
- All six standalone consumer workspaces pass locked dependency checks: five on
  Windows x64 cross-checks and the e2e fixture on Linux. Basic XLL also checks on
  Windows x86. The ABI-only probe needs no new dependency. The five consumers of
  xlfn only add ghost-cell to their lockfiles; no dependency upgrades are included.

## Boundaries and pending hosted verification

Windows e2e cross-check reaches the native blake3 build, which cannot complete on
this Linux host because `ml64.exe` is absent. Full Windows runtime tests, native
XLL loading in Excel, the full feature-powerset, Verus and hosted benchmark gates
are not implied by the local checks. Check CI on the actual PR head, not the
initial WIP checkpoint. No CI threshold or test assertion was weakened.

UI diagnostics were inspected before accepting updated snapshots. The registry
lifetime example still rejects an operation-local registry with E0597; direct
CallStorage construction now explicitly rejects its private constructor (E0624).
Other changes remove obsolete RefCell-specific non-Sync explanations while
preserving the remaining non-Send/non-Sync and conversion-trait rejection.

## Layout

On x86_64 the all-handle call owner is 96 bytes versus the prior 104-byte
CallScope. The decoder and plain ArgumentContext stay within the prior 80-byte
context budget. GhostToken is zero-sized. The assertions are configuration- and
architecture-specific, not a stable ABI promise. Existing permit allocation
policy is unchanged; the first retained permit remains inline.

## Performance

Prebuilt binaries were alternated for three baseline/new rounds, pinned to CPU 2,
with no concurrent compilation, 1 s warmup, 30 samples and 10 s measurements.
Direct Criterion timing estimates are compared, not its persisted-baseline
change output. Whole handle-call resolution is measured, not RefCell alone.

| Handles | Round | Baseline | GhostCell | Difference |
| --- | --- | --- | --- | --- |
| 8 | 1 | 638.93 ns | 651.06 ns | +1.90% |
| 8 | 2 | 648.58 ns | 648.01 ns | -0.09% |
| 8 | 3 | 616.93 ns | 638.06 ns | +3.43% |
| 32 | 1 | 3.3020 us | 2.4872 us | -24.68% |
| 32 | 2 | 3.1676 us | 4.0190 us | +26.88% |
| 32 | 3 | 2.3957 us | 2.4413 us | +1.90% |

The initial unpinned 32-handle comparison showed approximately +50% latency.
It did not reproduce with a consistent sign. Large process-to-process
variability prevents establishing a regression, neutrality or improvement for
32 handles. Eight-handle differences range from -0.09% to +3.43%. No speedup
is claimed and no speculative inlining change was made. Hosted benchmark
gates on the final commit remain necessary. Machine-readable estimates are
in `perf-alternating-summary.json`.
