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

The gate covers `lifecycle.rs`, all lifecycle children, `runtime.rs`, all runtime
children including opening/rollback/shutdown/finalization, the shutdown proof
definitions, `runtime_components/return_protocol.rs`, and
`return_abi/ownership.rs`. Recursive source globs include newly added child files.
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

The broader subsystem work below remains explicitly unresolved. Passing this
inventory certifies classification of its enrolled named references, rather than
proving the absence of all protocol debt. New debt must be recorded with its
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

## Cross-subsystem survey and remaining scope

`--all` has been used to survey the other crate source trees. These concrete
sites are follow-up work, not silently included in the completed lifecycle gate.
They need semantic enrollment with their own negative callers and runtime
regressions before the project can claim the same coverage there.

| Follow-up sources / operations | Initial classification and remaining work |
| --- | --- |
| `cache/reclamation.rs::{extend,from_closed}`, `cache/resident_index.rs::{get,drop}` | Distinguish cross-domain drain acceptance, retirement capacity assumptions, and private payload disarm. Preserve initializer/TLS destruction deferral; do not infer grace period from a final pin alone |
| `subscription/runtime/termination.rs::drain_sink_users`, `subscription/budget.rs::worker_limit`, source/disconnect workers | Separate runtime termination outcome accumulation from fixed-budget construction assumptions and external worker/join barriers; verify every source is attempted after hook errors |
| `excel_rtd/windows/server_gate.rs::acquire_resource_teardown`, `server.rs::shutdown`, `event.rs::drain_with_com_dispatch` | Audit remaining server-phase assertions and join/event failure endpoints against COM apartment, reentry, and event completion contracts; Windows cross-compilation cannot validate live COM behavior |
| `async_udf/boundary/launch.rs::{fail,into_parts,drop}`, `async_udf/boundary.rs::build_task` | Distinguish private launch/body delivery disarm from source/generation/registry joining and user-future destructor panic; keep cancellation atomic races dynamic |
| `registration/host.rs::mutation_from_invocation`, `registration/ledger.rs::new` | Decoder-executed `unreachable!` and uniquely owned normalization assumptions need narrow observed-result/normalization owners or explicit local recovery classification; retain host Applied/Rejected/Indeterminate outcomes |
| `diagnostics/worker.rs::{drop,shutdown}` | Classify observer/worker lifetime invariant separately from self-join and worker-panic outcomes; preserve consuming queue/delivery credits |
| `return_abi/numeric/process.rs`, `numeric/process/windows.rs::{attach,view}` | Fixed 64-byte ABI layout assertions and mapped-view validity belong to shared/raw ABI validation; private mapped-resource disarm and FP12 charge transfer need separate ownership review |
| `xlfn-package/src/distribution.rs::retry_windows_path_operation` and journal helpers | Bound-loop `unreachable!` is a local proof candidate; durable namespace/journal transitions and filesystem identity/content checks remain dynamic and require recovery tests |
| `xlfn-macros/src/codegen/udf.rs::emit_excel_function`, `model.rs::analyze` | Validated-IR expectations can move into constructor-limited IR; parser/type/content rejection remains dynamic. Quoted output must also be checked in emitted-code fixtures |
| `xlfn-kernel/src/drain_gate.rs::active` and counter/quota primitives | Overflow/underflow and raw atomic representations remain low-level checks. Do not replace shared atomic state with a static whole-object phase |
| Remaining handle, callback, ingress, package staging/PE, and benchmark/refinement support source | Surveyed by the lexical command only; not yet given occurrence-specific semantic reviews. External/raw input validation and benchmark-only assumptions must be separated before enrollment |

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
