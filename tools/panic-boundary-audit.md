# Panic consumption audit

All production boundaries that convert, ignore, or otherwise consume a panic
use `panic_boundary::catch_no_unwind`. Raw worker join results enter
`panic_boundary::contain_panic` before being discarded, matched, or converted
to a framework error. Exact `String` and `&'static str` payloads are destroyed;
other payloads are intentionally retained, including their allocations and
resources, so arbitrary payload destructors never run at these boundaries.

The scope is synchronous and asynchronous UDF calls, generated Excel/COM
exports, diagnostics, cancellation, cache teardown, handle destruction,
registration, lifecycle rollback/recovery, subscriptions, and RTD shutdown.
Async evaluation consumes each poll panic before dropping its poll wrapper or
taking the cancellation branch. Generated non-unwinding exports wrap their
entire bodies, including admission and guard destruction, in the common policy.

This handles ordinary unwind panics. It does not handle `panic=abort`, panicking
panic hooks, or double panics that abort before reaching the catch boundary.

## Direct-catch exceptions

`panic_boundary_allowlist.json` contains every direct Rust `catch_unwind`
reference with its exact path, lexical function/module and attribute context,
code line, occurrence count, category, and reason. Imports are inventoried as
well, so importing the function under an alias requires review. Test files and
macro expansion token streams have no whole-file exemptions.

The only production direct catches outside the helper preserve the original
payload for `resume_unwind`:

| Boundary | Reason and eventual consumer |
| --- | --- |
| `return_abi::destroy_thread_local_return_block` | Poison partially destroyed TLS storage, then resume into `free_return_boundary`. |
| `subscription::runtime::connect_transaction` | Roll back the reserved connection, then resume into the caller's UDF/COM boundary. |
| `subscription::data_plane::drive_notification` | Record the failed callback, then resume into the caller's boundary. |
| `rtd::windows::server::deferred_termination_worker` | Complete phase/refcount cleanup, then resume to its thread's join result. `TerminationWorker::join` consumes the payload. |

The remaining references are test observers of deliberately propagated,
fixture-owned standard string panics or assertions that a consuming boundary
does not unwind. One standalone cache-node layout experiment retains direct
catches for its fixed string Drop/warmup fixtures; it is not framework code.

## Thread join audit

The following framework owners consume raw join results through the helper:

| Owner | Required progress after a panic |
| --- | --- |
| Async executor startup rollback and close | Join every worker and preserve the worker-failure outcome before releasing shared execution state. |
| Diagnostics worker close and Drop | Complete worker ownership cleanup and report failure without running custom payload Drop. |
| Publisher-only and producer/publisher channel subscriptions | Join both worker roles even if the first panics, then return the subscription cleanup error. |
| RTD termination worker | Publish `Joined`, notify all joining waiters, and return `WorkerPanicked`. Callers receive a typed error, never a raw payload. |
| Benchmark support owners | Join owned workers while discarding or asserting their typed outcomes; a hostile raw payload is consumed first. |

Other `.join` occurrences were checked: path/string joins do not carry panic
payloads; thread joins in tests and the standalone cache-reclamation benchmark
are fixture assertions. Production RTD callers invoke `TerminationWorker::join`
and already receive `Result<(), ServerCloseError>`.

## Regressions and guard

`PanickingPayload` itself panics in Drop and records whether it was destroyed.
Regressions assert that the destructor count stays zero while sync/async error
delivery, return storage poisoning, remaining handle reclamation, cancellation
and disconnect phases, notifier Drop, COM error return, and RTD join completion
still proceed. Non-unwinding `extern "system"` regression entrypoints cover
value/status/void generated-boundary helpers and COM dispatch; any escaping
panic aborts that test process instead of producing a false passing result.

`just panic-boundaries` runs lexical-guard unit tests and compares current source
to the reviewed inventory. New references, moved references, changed test/cfg
contexts, changed counts, and stale allowances fail. Comments, strings, raw
strings, byte strings, and character literals do not create false references.
The check is a review gate, not a substitute for inspecting control flow or
testing unwind and ownership behavior. A change to a resume boundary's cleanup
must be reviewed even when its direct catch line remains unchanged.
