# Follow-up quality review (2026-09-07)

This review started from `026d150` and follows the earlier
[soundness audit](soundness-audit-2026-09-07.md). It covers the synchronization
kernel, cache and handle reclamation, subscription and async lifecycles,
diagnostics, Excel value/reference conversion, procedural macros, and package
validation. Independent follow-up review checked the resulting ownership,
locking, and error paths.

## Findings and repairs

| Finding | Repair and regression evidence |
| --- | --- |
| A reader delayed across two rotations could enter a reused generation before it became current. | Publish the current generation before reopening its gate. Production and Loom share this order; deterministic and Loom tests fail when the old order is restored. |
| Multi-area Excel references derived their data pointer through the ABI's one-element array field. | Project the variable-length tail through a raw pointer, preserving the complete allocation's provenance. A two-area Miri test reproduces the old invalid retag. |
| Closing could seal publication after call admission but before async execution-lease acquisition, causing an invariant abort. | Return `Closing` through all four async launch paths. A deterministic regression checks rejection and the continued validity of existing calls and leases. |
| Dropping an RTD refresh batch lost its retry notification, and planned-refresh cleanup could propagate a second panic. | Restore delivery state and drive the notification inside the shared panic-containment boundary. Tests cover retries and hostile notifier payloads during caller unwinding. |
| Disconnect could remove an owned subscription before discovering that its server was closing. Stale rollback could remove a newer connection. | Serialize publication metadata and subscription ownership with the same lock; rollback only removes matching generations. User shutdown callbacks run after unlocking. |
| Final subscription-runtime destruction could omit producer shutdown and invalidate pointers into the exclusively borrowed parent. | Run the normal close protocol during destruction. Keep the gate, quotas, and observations in one independent `PublishedOwner<RuntimeServices>` until producers and servers are destroyed. Miri covers queued values and sink calls during cancellation. |
| Subscription trace events could omit additions or record removals for uncommitted connections that were never counted. | Record installed subscription ownership, with exactly one removal for rollback, disconnect, or termination. Four cleanup paths and existing Lean trace checks cover the accounting. |
| Cache initialization could leave reclamation debt after a callback dropped a lease, particularly with zero capacity or a panic. | Perform maintenance after successful and error-returning callbacks. An unwind guard attempts idle reclamation after initializer and singleflight guards are released, containing destructor panics and preserving the original panic. Active initializers and readers defer reclamation. A bounded regression also rejects waiting for an outer reader during unwinding. |
| Separate diagnostic writers retained obsolete file handles and sizes after another writer rotated the log. | Open the current log and read its size under the shared file lock for every write. A multiple-writer regression checks archive contents and size bounds. |
| Deep PE import and forwarded-export chains consumed unbounded call-stack depth. | Use iterative graph traversal, retaining complete error paths and sharing visited state. Tests exercise 4,096-node chains and cycles on a 128 KiB stack. |
| An empty `RUSTC_WRAPPER` was treated as an executable path. | Interpret it as disabled and launch the compiler directly. A command-construction regression covers the empty value. |
| Raw Rust identifiers were rejected as default macro IDs or leaked their `r#` prefix into worksheet names. | Normalize identifier-derived defaults while preserving explicit string options. Unit and executable compile-pass tests cover functions, arguments, add-ins, and enums. |

The guide now describes dependency-name resolution and raw-identifier defaults.
Contributor commands match the serial libtest invocation used by Windows CI.
RTD Miri regressions are included in the shared `just miri` recipe.
Custom unsafe RTD implementations now have an explicit sink-shutdown obligation
on normal return, `Err`, and unwinding, matching the runtime's existing policy.

## Validation

These results describe the reviewed revision. Subsequent cleanup removed the
experimental sharded backend and its Miri recipe; the production cache protocol
regressions remain, and the dependency limitation below still applies.

The final integrated checks passed:

| Check | Result |
| --- | --- |
| Workspace tests, all features, locked dependencies | Nextest: 785 passed, 7 skipped. Serial libtest and documentation tests: 785 passed, 7 ignored. |
| Feature powerset, depth two (`just features`) | All 36 configurations passed. |
| Workspace Clippy, all targets and features | Passed with warnings denied. |
| Windows production-code Clippy, all features | Both `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc` passed with warnings denied and `blake3/pure`. |
| Standalone example and E2E-fixture Clippy | `basic-xll`, `rtd-source`, and `xlfn-e2e-fixture` passed with warnings denied. |
| Selected temporal-safety Miri suite (`just miri`) | Passed under Stacked Borrows and Tree Borrows, with leak and alias validation enabled: kernel 17, handles 11, async 10, RTD 6 test executions per model. Feature suites overlap. |
| Two new cache-unwind backend regressions | Both passed under both Miri borrow models with leak and alias validation enabled, using the sharded Miri backends. The native cache suite passed all 66 tests. |
| Ignored Shuttle handle-insertion/close regression | Passed when explicitly selected. |
| Lean models and subscription trace checks | `lake build` passed (80 jobs); all six shutdown/composition trace tests passed. |
| Panic-boundary audit | Five checker tests passed; all 44 direct catch references matched reviewed entries. |
| Dependency policy and API compatibility | `just deny` passed all four policy checks. `just semver` passed all 196 checks for the unchanged compatibility line; version-permitted breaking changes were skipped by the tool. |
| Documentation and formatting | Rustdoc passed with warnings denied; mdBook and all 29 guide chapters passed their checks; formatting and whitespace checks passed. |

All 7 tests ignored by the ordinary test run were exercised separately: six
Lean trace tests and the Shuttle regression. No Windows or Excel execution is
included in these local results.

## Evidence limits

The local host is macOS arm64. Windows cross-compilation checks cannot establish
linked Windows execution, SDK ABI validation, or behavior in live Excel.

Lean's temporal-reclamation model verifies abstract admission, observation,
pinning, and reclamation conditions. It does not model the concrete two-gate
publication order; the new deterministic and Loom tests cover that boundary.

The existing [full-cache Miri limitation](../crates/xlfn/benches/experiments/cache-miri.md)
in Moka/Crossbeam remains separate from the framework regressions. Passing
selected Miri tests is not a claim that the complete dependency stack is
Miri-clean. No finite review establishes the absence of all future improvements.

The cache unwind guard drains the affected cache's retired nodes when its
readers are idle and no initializer is active. Otherwise, subsequent operations
retain the existing reclamation obligation. If an
initializer drops a lease belonging to a different cache, that cache's next
operation, explicit `clear`, or destruction remains its reclamation trigger.
The global initialization-depth guard avoids invoking value destructors under
another initializer's singleflight lock; no unsafe cross-cache cleanup queue
was introduced.
