# Protocol premise review

The review target is ownership and ordering, rather than the number of runtime
checks. A phase snapshot is an observation; it is not permission to publish,
withdraw, reclaim, or report successful shutdown. Conversely, a Rust owner cannot
make an external callback succeed or make a shared atomic count stop changing.

The maintained inventory is [`tools/protocol_invariants.json`](../tools/protocol_invariants.json).
Each semantic review records the proposition, establishing operation, current
owner, implementation layer, disposition, retention reason, relevant tests, and
exact source occurrences. A `protocol_debt` entry additionally requires a concrete
receiver/payload alternative. References to tests describe the intended regression
coverage; this file is not an execution report or Windows/Excel qualification.

## Enrolled boundary

The gate covers every `crates/*/src/**/*.rs` source, including kernel primitives,
the runtime and return ABI, subscription/COM, cache, handles, diagnostics,
registration, macro generation, shared validation, raw ABI resolution, packaging
and the CLI. Recursive source globs include newly added crates and child files.
It covers production-feature branches, including `refinement`, regardless of the
features selected on the auditing host.

Pure `cfg(test)` items are excluded from the production inventory. An external
module file is excluded only when all its resolved incoming module declarations
are test-only (or descend from such a file). Names such as `tests.rs` and
`test_support.rs` confer no exemption. Uncertain `#[path]` declarations are not
used to prove a file test-only. `cfg(any(test, feature = "refinement"))` remains
enrolled. The broader
survey also includes benchmark/refinement and proc-macro token-stream source;
its counts must not be presented as counts of shipping runtime checks.

| Disposition | Meaning | Required treatment |
| --- | --- | --- |
| `observation` | Diagnostic, data classification, or canonical projection | Must not issue a transition or completion capability |
| `acquisition` | Obtain a current decision from shared/external state | Check under the issuing lock or explicit admission boundary |
| `revalidation` | Accept an owner after callbacks, waits, reopen, or independent-input joining | Validate issuer, attempt/epoch, and current external facts before effects |
| `exhaustion` | Finite identity/count arithmetic cannot advance | Retain checked arithmetic and fail closed without reuse |
| `low_level` | Raw representation, atomic publication, or terminal unsafe-invariant endpoint | Keep within the audited implementation layer |
| `local_recovery` | Private move-out/Drop disarm or panic recovery state | Do not mistake an armed Option for a cleanup/drain certificate |
| `protocol_debt` | Receiver still permits a locally inconsistent protocol combination | Record the narrow receiver/payload replacement; do not relabel it as unavoidable |

## Changes to the trusted boundary

The following changes remove or relocate premises; they do not merely rename the
old assertion.

| Old premise | Establishing operation and owner now carrying it | Regression boundary |
| --- | --- | --- |
| Independent idle/opening phase checks before allocating an open attempt | One locked closed-idle acquisition issues `OpenStart`; abandonment quarantines its own coordinator | `lifecycle_phase_is_not_open_start`, opening state tests |
| A stored raw removal-attempt integer is checked for zero when issuing a claim | Canonical storage keeps the existing nonzero-backed `RemovalAttemptId`; checked successor still rejects exhaustion without identity reuse | `exhaustion_never_wraps_to_an_old_removal_identity` |
| Standalone `OpenStart` disarm | `transfer_to_opening(self, OpeningTxn<Begun>)` validates issuer/attempt and transfers to that concrete successor; failure retains both owners | `lifecycle_open_start_without_successor`, `lifecycle_open_start_unit_successor`, `lifecycle_open_handoff_recovery_private`, `lifecycle_open_handoff` |
| Opening commit followed by an Open-phase/generation postcondition resnapshot | The locked commit directly issues `CommittedOpen`; the observer consumes it | `lifecycle_commit_observation_without_proof`, `lifecycle_commit_proof_forgery`, `lifecycle_commit_guard_reuse` |
| Opening abandonment leaves return admission open | `abandon_open` closes admission before retaining/quarantining its resources | Opening abandonment and forget regressions |
| Final-close classification changes state and then re-matches it with a fail-stop fallback | The canonical transition returns its typed closing decision directly; only the empty-claimed receiver can finish closed | Final/rollback decision, wrong-purpose, and `lifecycle_broad_finish_closed` fixtures |
| Two possible authorities in a removal guard; combine-returned-authority panic/double-return checks | Canonical storage has the sole `Outstanding` or `Returned` owner; a return moves authority immediately under the same claim acquisition | Claim-return and quarantine-request regressions |
| Broad authority installation re-matches an already occupied or incompatible recipient with fail-stop branches | Private `PreparedAuthorityInstall::acquire` joins one empty canonical slot and authority epoch; `VacantClose` has only unclaimed or outstanding-claim receivers and consuming install cannot represent existing authority | `lifecycle_authority_install`, `lifecycle_authority_install_forgery`, authority-slot rejection/retention regressions |
| Cleanup returns Missing, then caller reacquires canonical access and rechecks module/ingress state | One canonical cleanup acquisition qualifies `NoModuleAuthority` against the separate ingress domain, or returns `Unavailable`; recovery consumes that result | Quarantine and foreign-access authority regressions |
| Fresh closing decision is issuer-checked again before claim construction | The redundant check is removed; one private `RemovalOwner::new` checked join accepts independently obtained dependencies/claim before constructing the owner | `abandoned_acquisition_returns_authority_to_its_original_issuer` |
| Broad teardown stage plus separate `BindingResponsibility` enum require fail-stop matches | The recovery-stage trait's binding type associates installed responsibility before cleanup and mandatory `EmptyLifecycle` after cleanup; failed cleanup retains a separate typed terminal owner | `teardown_binding_phase`, `teardown_sealed_binding_phase`, `teardown_cleaned_binding_phase` |
| Quarantine tries to seize resources from an active opening/cleanup owner | Requests are deferred; only a ready resource receiver moves immediately, and owner resolution applies the request | `miri_quarantine_request_waits_for_the_unpublished_opening_owner`, `miri_quarantine_request_retains_published_opening_until_owner_resolution` |
| Completed-removal epilogue rechecks phase/intent using a broad receiver | `CompletedRemovalIntent` acquires the exact closed-idle/explicit-completion state and holds the guard through consuming clear | `lifecycle_removal_intent_without_claim`, `lifecycle_removal_intent_twice` |
| One pending finalization receiver contains both certified and already-logical variants | Pending completion contains only its certified binding; private logical/certified failure owners expose no finish operation | Finalization release failure, unwind, abandonment, forget, and post-release quarantine regressions |
| Wait, observe idle bool, then call a generally available return certificate constructor | `ClosedReturns::wait` produces an issuer/epoch-bound drain; acceptance validates the current protocol epoch | `return_protocol_raw_tracker`, `return_seal_wait_twice`, `return_protocol_drain`, return protocol regressions |
| Shutdown orchestration can call generic proof `issue` helpers | Add-in/generation proofs are issued inside the pipeline operation module; subscription stop proof is issued only by the actual service seal | `shutdown_certificate_issuance`, `shutdown_subscription_issuance` |
| Empty handle-service proof manufactured without canonical service acquisition | `HandlesSealed::absent_services` checks absence while holding canonical access; present services seal under that same access | `shutdown_handle_empty_issuance` |

Return drain is deliberately not a permanent fact. Its stamp is globally
non-reused, including when storage is reused or a protocol moves. Waiting holds
the admission management mutex to exclude reopen, while final release uses its
own notification path. Acceptance checks issuer address, stamp, sealed admission,
and idle state again. Pipeline proof acceptance additionally binds coordinator
and removal attempt. A reopen invalidates even an already-certified completion.

The opening ingress domain has a separate atomic phase and opening mutex.
`Ingress::complete_open` drains export users, checks its own Opening phase,
executes publication, and then performs its Opening-to-Open CAS. Its failure
endpoint is retained: an outer failure can follow publication, so ordinary
rollback to a supposedly untouched earlier stage would be incorrect. This is
distinct from the removed canonical same-lock successful-commit resnapshot.

## Retained checks and scope limits

The live ledger groups exact source sites under these semantic decisions:

| Review IDs | Established fact / retained boundary |
| --- | --- |
| `LC-RAW`, `LC-PUBLICATION`, `OPEN-ATOMIC-PUBLICATION` | Valid raw enum representation, drained canonical pointer projection, and the separate ingress-domain Opening-to-Open atomic publication/CAS; retained in their implementation layers |
| `LC-EXHAUST`, `RETURN-EXHAUST` | Checked, non-reused identities; finite exhaustion remains dynamic |
| `LC-OPEN-ACQUIRE`, `OPEN-REVALIDATE`, `OPEN-HANDOFF-IDENTITY` | Current opening attempt, callback/close outcome, and original-start/concrete-successor identity acquisition; old successful-commit postcondition check is removed |
| `LC-CERTIFICATE`, `LC-RETURN-CLAIM` | Reacquire the shared canonical recipient and validate issuer/attempt/epoch after unlocked work |
| `OPEN-RAII`, `RETURN-RECOVERY`, `SHUT-RAII` | Private move/disarm state, not permission to claim cleanup success |
| `ROLLBACK-IDENTITY`, `LIFECYCLE-BINDING` | Independent-runtime joining and dynamic thread-affine/TLS access acquisition |
| `SHUT-ACQUIRE`, `SHUT-ISSUER-ACQUIRE`, `MODULE-CLEANUP-ACQUIRE`, `SHUT-PROOF`, `SHUT-HOST`, `SHUT-DESTRUCTOR` | Current cleanup/issuer acquisition, qualified canonical/global module cleanup, post-wait proof acceptance, host results, and partially progressed destructor outcomes |
| `RETURN-GATE`, `RETURN-EPOCH`, `RESIDENCY`, `RECOVERY-EPOCH` | Shared count, current epoch, native-resource/policy qualification, and retained-undrained authority acceptance |
| `LC-OBSERVE`, `OBSERVATIONS`, `REFINEMENT` | Read-only projections and feature-gated diagnostic/refinement backend assumptions |
| `RECOVERY-HAZARD` | Fail-stop endpoint for an already classified unsafe unload invariant; ordinary external cleanup failure quarantines |

Passing this inventory certifies classification of its enrolled named references,
rather than proving the absence of all protocol debt. New debt must be recorded with its
current owner, exact source sites, concrete replacement receiver, and relevant
regressions. Removing a check requires updating its semantic decision and adding
a test for the formerly permitted misuse; deleting the inventory reference alone
is not remediation.

The `AlreadyClosed` boundary uses a phase snapshot to decide whether to attempt
an idempotent empty-binding release. That snapshot does not issue a close
certificate: the binding operation still acquires and checks its actual empty
claim. Such a hint is distinguished from permitting reclamation based solely on
a phase observation.

`PreparedAuthorityInstall` illustrates a manually reviewed boundary that is not
found by a guard-name search: its acquisition uses enum patterns and direct epoch
comparison. Its private parts retain both the acquired receiver and authority;
normal abandonment, panic, or forget cannot drop a published payload before
drain. Only consuming install releases the retained parts into the canonical
recipient. The authority-slot regressions cover occupied/foreign-epoch rejection,
finalizing/foreign-access rejection, and abandonment of a published payload.
`prepared_install_abandonment_never_destroys_a_published_payload` intentionally
retains the native test's published roots; the separate
`miri_abandoned_prepared_install_retains_then_recovers_staged_payload` fixture
uses a staged payload and test-only no-reader recovery rather than disabling
Miri's leak checks.

## Ownership conservation

Every active opening/removal authority is held by canonical storage or a concrete
opening/rollback/teardown owner. Quarantine requests do not independently acquire
that authority. Returning an owner selects the outstanding canonical claim under
the lock, checks its identity, and moves the authority there; a second local copy
is not retained by the guard.

Ordinary abandonment and panic retain/quarantine resources before releasing a
claim. Once destructor execution has begun, failure retains an empty binding and
a terminal failure fact rather than returning a supposedly untouched lifecycle
value. Boundary success requires the same completion owner to finish logical
close, release its binding, and complete the canonical finalization step in the
specified order. A post-release quarantine request can still prevent success.

`mem::forget` does not run recovery and may retain a lock, claim, or resource
indefinitely. This is a liveness/leak outcome, never permission to reclaim early
or manufacture success. Merely hiding `reclaim()` is insufficient if field Drop
would still destroy an undrained payload. The relevant source owners and runtime
regressions, rather than this lexical audit, enforce those rules.

## Cross-subsystem dispositions

The former follow-up table is now enrolled occurrence by occurrence. The JSON
records both removed premises and retained dynamic boundaries; the rows below
identify what changed and why the remaining checks stay. The inventory includes
benchmark/refinement code and conservative test-file inclusions, so its total is
not a count of shipping runtime checks.

| Sources / operations | Implementation and retained boundary |
| --- | --- |
| Cache reclamation and resident index | `ClosedCacheDomain::drain(self)` derives its own domain and returns an opaque batch; callers cannot combine another domain/certificate/batch or extract raw disposable storage. Index snapshots remain non-owning, resident/flight pins remain affine, and initializer/TLS disposal deferral is preserved. Capacity and raw atomic conservation stay inside the backend. |
| Subscription termination and source/disconnect workers | Failure recording returns `XllError` directly and preserves the first failure under one mutex. Stop/join continues across hook errors; preflight-not-executed and barrier-executed-with-error remain distinct. `CatalogEntry` retains its arena and validated index. Channel sources retain the nonzero worker budget directly. Shared worker outcomes and external barriers remain dynamic. |
| COM server teardown and event dispatch | Resource teardown consumes `DrainedServerOperations` instead of asserting phase on a broad gate. The same owned server reference retains COM lifetime across the staged operation. Apartment/message dispatch, self-join, event completion, and external COM failure are still checked at their actual boundaries. |
| Async launch/task construction and cancellation | Consuming launch/task owners already retain source, generation and response responsibility. Private body/registration `Option`s are move-out/Drop recovery, not issuer selectors. Cancellation/delivery CAS and user-future destructor outcomes remain dynamic. |
| Registration result decoding and ledger normalization | `HostInvocation::Decoded` requires a decoder result; `Failed` requires private failure status. Success without a decoded payload cannot be built. Cleanup still preserves Applied/Rejected/Indeterminate. Fresh `ExcelNameKey` normalization keeps its local unique-Arc check: it is construction bookkeeping, not host cleanup evidence. |
| Diagnostic worker lifetime | Private `DiagnosticWorker` keeps observer allocation and join handle together; consuming join produces the joined owner before observer extraction. Ordinary Drop joins, while forget retains both. Self-join and worker panic remain runtime results. Queue/delivery credits retain their existing consuming transfer. |
| FP12 accounting and Windows mapping | `Mapping` contains a non-null view after successful map. `initialize_and_retain(self)` validates/initializes the exact view before a process-lifetime reference escapes; failure runs mapping/handle cleanup. The old optional-view expectation is gone. Shared `ProcessState` remains a 64-byte data-only ABI, and publication still consumes its charge. |
| Package retry, journal, staging and PE | Retry retains its actual last result instead of reconstructing loop completion with `unreachable!`; regression fixes retry count and error Drop ordering. Journal recovery still re-reads durable state, namespace identity/content and external filesystem results. Caller-supplied validation bypass remains closed. |
| Macro validated IR and emitted wrappers | Validated context bundles Addin/type, and `NativeValueAbi` couples conversion to its native ABI. Codegen uses validated payloads rather than reconstructing boolean/Option premises. Metadata length derives from its owner. Parsing and source-content rejection remain dynamic; emitted wrapper fixtures cover the generated boundary. |
| Kernel gates, rotating domains and service slots | Shared atomic counts, notification tails, finite overflow and issuer acceptance stay inside audited primitives. Service-slot initialization/sealing now classifies owned state once under the lock and moves FnOnce callbacks directly; callback return/panic still reacquires the canonical recipient. |
| Handle publication, topic retirement and pending leases | Publication consumes its own reservation/token. Topic owners cannot undergo ordinary payload Drop before grace; table destruction and bulk retirement drain their own domain before producing `DrainedTopics`. A pending lease binds only after release-build generation validation, and its pin/issuer fields are private. |
| Callback results, ingress and argument finalization | Fresh callback status directly issues readable/cleanup-only ownership; release remains consuming and disarms before FFI. Ingress invokes FnOnce hooks directly and its drain proof cannot be forged by sibling callers. Scope-only formula arguments return an explicit missing-admission error; admitted frames retain required RTD access directly. Raw external operands and the separate atomic ingress publication/CAS remain checked. |
| Common names, raw ABI, input identity and benchmark/refinement support | Naming, UTF16, matrix bounds, PE and external pointer checks validate actual data, not lifecycle stages. Pure encoder representation checks remain local. Benchmark setup/simulated completion and conservatively included test files are explicitly labelled, without treating them as shipping cleanup checks. |

Compile-fail fixtures test privacy and invalid phase/owner combinations from the
consumer side, with exact diagnostics and positive controls. Runtime regressions
cover live-reader abandonment, panic and forget retention, unlock-before-payload
destruction, worker/observer lifetime, callback cleanup, and retry error ordering.
These tests complement the semantic inventory; a passing lexical audit alone
does not establish them.

Pending-lease binding checks generation-value equality before exposing the
payload. That comparison alone does not prove runtime-instance identity;
provenance is retained by the checked decoder/launch ownership path. Neither a
shared lifetime parameter nor equal numeric generations independently supplies
that proof. The check runs at the beginning of the generated async body, before
context construction and user code. Unpolled cancellation retains the pending
pins until future destruction. This does not establish zero layout/performance
cost for the generated coroutine.

The broad survey is reproducible from source; its generated JSON belongs under
`target`, rather than a machine-specific tracked report. The enrolled JSON is a
reviewed maintenance artifact, not benchmark data or an exploratory test dump.

## Maintenance and limitations

Run the focused review and normal Python checks from the repository root:

```sh
just protocol-invariants
just python-check
uv run --project tools --locked python -B -m xlfn_tools.check_protocol_invariants --list
uv run --project tools --locked python -B -m xlfn_tools.check_protocol_invariants --all > target/protocol-survey.json
```

New or removed enrolled occurrences, statement changes, and moves to another
path or named context fail the gate. Review the
actual proposition, establish its owner/operation, and update the appropriate
semantic entry or add a new one. The tool has no update/auto-approval command.
Keep test names and receiver alternatives current. A protocol guard moved into a
helper still needs a classification; a smaller count is not evidence of a
stronger invariant.

The scanner uses the shared Rust comment/string lexer and keeps literal contents
in source anchors so changing an `expect` premise is visible. It inspects macro
token bodies and recognizes local imports that alias the guarded names. It
normalizes whitespace, records function/module context and multiplicity, and
rejects stale/duplicate references. Unit tests fix these behaviors, including
predicate-condition changes and nested `cfg` handling.

Reordering identical statements within the same function does not change their
anchors or multiplicity; the lexical gate cannot establish ordering correctness.
Runtime, compile-fail, and formal checks still cover that property.

This is a lexical introduction guard, not Rust name resolution, control-flow or
dataflow analysis, a type checker, or a conservation proof. It inventories named
guards, panic/assert/expect/unwrap/unreachable/abort/fail-stop calls, and selected
predicate methods (`phase`, `status`, `is_*`, `has_*`, `can_*`, identity and return
acceptance methods). A raw boolean field, direct enum match, arbitrarily renamed
wrapper, function-pointer indirection, or check introduced outside the enrolled
globs can require manual discovery. String/character tokens are excluded as
executable candidates, not as premise text. Changing a helper's implementation
without changing its call site still requires normal code review and tests.
Module-path changes also require review: the file-exclusion inference follows
ordinary module declarations, not custom `#[path]` aliases or macro-expanded
module graphs. In particular, an alias to a file also reached through a test-only
ordinary declaration needs manual inspection. A filename alone never exempts a
file, but this lexer does not establish complete compiler module reachability.

Compile-fail fixtures must run at the actual consuming sibling boundary and check
the intended diagnostic. Runtime tests must cover foreign issuer, stale epoch,
callback/destructor errors, abandonment, panic, and forget where applicable.
Formal refinement and Miri/Loom validate different properties and stay separate
checks. This audit does not replace them and does not attest to a remote CI run.
