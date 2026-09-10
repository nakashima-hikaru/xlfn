# Soundness and maintenance audit — 2026-09-07

This audit starts from `7889c9c` and covers the workspace, including the kernel,
framework, procedural macros, ABI layer, common validation, package inspector,
CLI, benchmark support, CI, and verification documentation. After addressing
the supplied three findings, separate reviewers examined publication,
reclamation, async scheduling, application callbacks, and public capability
contracts again. The resulting changes are described below so reviewers can
check the obligations across files.

## Supplied findings

| Finding | Resolution | Regression evidence |
| --- | --- | --- |
| Consumed panic payloads could panic again during destruction | Use `catch_no_unwind` or `contain_panic` at every consuming boundary, including raw thread joins. Generated Excel exports wrap admission, work, and guard destruction. COM entrypoints use the same policy. | Hostile `PanickingPayload` tests for sync/async UDFs, free callbacks, handles, subscriptions, COM, and RTD termination; generated-wrapper AST checks; occurrence-specific CI inventory. |
| Gate waiter and active counters could miss each other | Encode `SEALED`, `WAITING`, and the active count in one atomic state. A final release with a notification obligation retains its count until it owns the wait mutex. Waiters register under that mutex through the same atomic RMW protocol. | Shared production transitions exercised by Loom; concurrent final-release and immediate-owner-reclamation Miri probes. |
| Cache capability auto-traits were weaker than their contracts | `CacheLease<V>: Send` requires `V: Send + Sync`; internal node-pointer traits require both shared access and cross-thread destruction to be valid. | Static assertions cover ordinary values, `Cell`, `Rc`, and a `Sync` but non-`Send` guard type. |

The [panic audit](panic-boundary-audit.md) records the four intentional
production resume-only boundaries and the raw thread-join inventory. Custom
panic payloads are deliberately retained; this can retain their allocations
and resources. Aborting panics and double panics before reaching a catch
boundary remain outside this policy.

## Additional lifetime and concurrency findings

| Area | Failure mechanism | Final ownership or ordering rule |
| --- | --- | --- |
| Published allocations | Moving a `Box` after publishing a pointer can invalidate the pointer's borrow tag even though the address stays stable. Miri reproduced this in framework-owned storage. | `PublishedOwner<T>` converts the Box to unique raw ownership before publication. Moves affect only the owner pointer. Recovering a Box or destroying the owner occurs after raw capabilities drain. |
| Async generation snapshot | Generation advance could reclaim a generation between a raw pointer load and admission. | Acquire a generation pin while holding the short publication mutex, before releasing the snapshot protection. |
| Canceled async tasks | Removing cancellation controls reduced `task_count` to zero while a task's completion guard still accessed the generation. | Independent pins cover reservations and completion guards. Cancellation control count is not reclamation authority. |
| Async teardown | Dropping an executor on an early-return or unwind path could free state while workers still ran. | Executor Drop cancels, drains, seals scheduling, and joins every worker before destroying shared state. Reservation fields release admission, generation pin, and executor lifetime protection in order. |
| Async worker wakeup | A worker's queue recheck and a producer's idle-mask load could both miss the other publication. | Queue publication and idle announcement participate in the same AcqRel RMW order. A worker either finds the task or receives a park token. |
| Final gate release | Publishing zero before completing notification could allow reclamation during mutex/condvar access. A whole-gate reference spanning the final unlock also protected immutable padding after reclamation. | The slow path holds the wait mutex before publishing zero. Raw owned releases project only interior-mutable fields and perform no owner access after the final unlock. Borrowed permits retain ordinary Rust lifetimes. |
| Task control removal | Destruction of a cancellation source under a task-shard lock could invoke a reentrant application waker. | Remove and account for the control under the lock, then destroy it after unlocking. |
| Generation service slots | Publication and first-read acquisition were not atomic with sealing; owner movement also affected published pointers. Application clone/drop/callback code could run under the state lock. | Establish the first read while holding the state lock, publish through stable unique ownership, and snapshot application-owned fault/configuration values for work outside locks. |
| Call-scoped handle borrows | A publicly constructible call scope or a too-short domain borrow could permit a registry to die while its capability remained live. | Only the framework creates call scopes; the invariant call brand binds domain ownership to the entire generative scope. Compile-fail cases exercise both escape routes. |
| Returned arrays and counted strings | Deriving a buffer pointer before moving its Box into the return owner invalidated the pointer under Miri. | Establish `PublishedOwner` before storing the raw buffer pointer; preserve ownership through synchronous host consumption or the free callback. |
| Handle topic close | Publishing a topic into a reclaim queue before removing its lookup entry allowed a concurrent grace period to free discoverable storage. | Unpublish all entries under the table write lock before enqueueing any owner for reclamation. |
| Subscription operation guards | An unconditional unsafe `Send` implementation did not require the host's associated admission guard to be transferable. | Require `AdmissionGuard: Send` in the host contract and let the compiler derive the operation guard's `Send` implementation. |

`PublishedOwner` is used for executor and generation storage, lifecycle
generations, service slots, cancellation slots, cache domains and erased cache
owners, handle binding/object/topic owners, subscription servers, diagnostics
observers, returned buffers, and Windows callback records. It does not replace
the capability counters or make raw pointers safe by itself. Each subsystem
still owns its admission and drain proof.

## Input, error, and maintenance findings

- Reject full array-builder writes before invoking `IntoExcel`, encoding
  strings, or allocating arena storage. Failed writes leave cell and allocation
  accounting unchanged.
- Apply the same input-array budget to owned, borrowed, and formula input
  paths. Validate metadata against UTF-16 length/NUL restrictions and Windows
  module-name requirements before registration.
- Check pin and task identifiers transactionally; exhausted counters cannot
  wrap into a reused identity. Fix quota admission at `usize::MAX` without an
  eagerly evaluated overflow expression.
- Resolve named PE export forwarders, including aliases, so dependency-cycle
  checks see name-based forwarding edges as well as ordinal edges.
- Keep feature checks read-only by removing cargo-hack's manifest-mutating
  `--no-dev-deps` mode. Add an independent panic-inventory CI job and async Miri
  coverage. Align the guide with the limits of the formal model.
- Remove the workspace-wide semver `major` override, which skipped every
  compatibility lint even for unchanged-version crates. Derive the allowed
  changes from each crate's version and document when a new release baseline
  is required.

The removed call-scope constructor and strengthened capability trait bounds
are intentional corrections in the pre-1.0 API. Normal generated UDF call
scopes continue to be supplied by the framework. Invalid metadata now fails
earlier with a diagnostic.

## Verification scope

The kernel's one-counter notification model explores both open and sealed
admission without a preemption bound. The two larger gate models use a bound
of two preemptions. The existing rotation model and new queue-publication
model complement those checks. None substitutes for integration tests.

Miri probes cover owner movement, real return storage, binding retirement,
generation snapshots, canceled-but-running tasks, implicit executor Drop, and
the final release of owned capabilities. All selected probes run with both
Stacked Borrows and Tree Borrows. The two immediate-reclamation probes additionally
run 16 seeds per borrow model, eight iterations per test, using
`-Zmiri-many-seeds=0..16` with and without `-Zmiri-tree-borrows`.

The Lean checkers validate abstract lifecycle and ownership transitions and
Rust-produced traces. They do not prove Rust aliasing, atomic memory ordering,
COM behavior, or the implementation of dependencies.

## Recorded validation

These results refer to the completed changes on macOS arm64 (Darwin 25.6.0),
Rust 1.98.0 (`88d9e12ae`), and Miri on nightly 1.100.0 (`c656540d6`,
2026-08-21). Test counts overlap between commands;
they must not be added as independent evidence.

| Check | Result |
| --- | --- |
| `cargo nextest run --workspace --all-features --locked` | 756 passed, 7 dedicated tests skipped. |
| `cargo test --workspace --all-features --lib --locked -- --test-threads=1` | 705 passed, 7 dedicated tests ignored. |
| Dedicated ignored tests with both Lean checker environment variables set | All 7 passed: the Shuttle close race and the shutdown/composition trace tests. |
| `just miri` | 16 kernel, 10 handles-feature, and 9 async-feature executions passed. |
| `MIRIFLAGS=-Zmiri-tree-borrows just miri` | The same 16 + 10 + 9 executions passed. |
| Kernel immediate-reclamation probes with 16 seeds | Both probes passed with each borrow model, eight iterations per test. |
| Workspace all-targets/all-features Clippy, `-D warnings` | Passed. |
| Windows i686/x86_64 production-library all-features Clippy, `blake3/pure`, `-D warnings` | Both passed. This is compilation/lint evidence, not Windows execution. |
| Standalone basic example, RTD example, and end-to-end fixture Clippy | All three passed on the local host, including their targets. |
| Feature powerset, depth 2, `--lib --locked` | All 36 configurations passed. |
| `just panic-boundaries` | Five guard tests passed; all 43 direct references matched the reviewed inventory. |
| Warning-as-error workspace rustdoc, no dependencies | Passed. |
| `just deny` | Advisories, bans, licenses, and sources passed. |
| `just semver` | 196 compatibility checks passed for the unchanged-version common crate. The tool permits the explicit 0.1 → 0.2 transitions and skips their compatibility lints. |
| Lean checker build and checked-in fixtures | 107 build jobs completed; 10 positive/negative fixture inputs behaved as expected. |
| Guide build and validation | mdBook build passed; all 29 chapters validated. |

The Miri diagnostics fixture uses a fixed timestamp: checking owner movement
does not require reading the host's real-time clock or disabling isolation.

## Async performance check

The four production async CI cases were measured in separate release builds
of `7889c9c` and the corrected tree on the same macOS arm64 host. Each case
used ten seconds of measurement, twenty samples, and 0.5 seconds of warm-up.
The table uses serial runs with other verification processes finished. An
earlier baseline taken while other checks were active was repeated to avoid
comparing against that interference.

Values are Criterion slope point estimates for a whole batch, not individual
task latency. Every case uses four executor workers.

| Case | Tasks per batch | `7889c9c` | Corrected | Change |
| --- | ---: | ---: | ---: | ---: |
| `per_iteration/1` | 128 | 108.87 µs | 110.72 µs | +1.70% |
| `per_iteration/32` | 4,096 | 2.5905 ms | 2.5635 ms | −1.04% |
| `matrix_reschedule/workers_4/16` | 1,024 | 876.62 µs | 903.39 µs | +3.05% |
| `spawn_and_drain/workers_4/16` | 1,024 | 1.3943 ms | 1.4702 ms | +5.44% |

Reproduce with the `async_spawn` benchmark, features `bench-internals async`,
the four-case filter from `just bench-ci`, and
`--warm-up-time 0.5 --sample-size 20 --noplot`. The new publication lock,
generation pin, and notification handshake add synchronization; the local
comparison records its observed cost rather than claiming a speedup. These
four cases do not establish Windows/Excel performance or tail-latency bounds.

## Remaining verification limits

- The local host is macOS arm64. Windows x86/x64 production code is
  cross-checked, but linked Windows tests, the SDK ABI probe, and live Excel
  require their Windows environments. Cross-checking test targets locally is
  blocked by the missing Windows SDK header `malloc.h` in the `alloca` build.
- Full cache Miri execution reaches an upstream Crossbeam intrusive-pointer
  retag failure, independently reproduced with Moka alone. The same dependency
  path can be reached through async peer stealing.
  The async lifetime Miri regressions use one worker to isolate framework
  ownership; native integration tests still exercise multiple workers.
- Miri emits third-party integer-to-pointer provenance warnings from
  parking_lot/Crossbeam. The reported passing suites do not disable validation,
  enable permissive provenance, or suppress leak checking.

No audit or finite test run establishes that every future improvement has
been exhausted. The completion criterion here is that the concrete findings
from both review passes are repaired, have regression evidence where
applicable, and introduce no unresolved failure in the supported local checks.
