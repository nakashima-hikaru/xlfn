# Runtime simplification review — 2026-09-07

The cleanup preserves identity domains, affine teardown certificates, and the
resident → retired → reclaimed ordering. No configuration options were added
for tuning constants. Each stage is committed independently after formatting,
workspace all-target/all-feature Clippy, workspace all-feature serial tests,
and whitespace checks.

## Completed stages

1. Removed the sharded resident-index experiment, executable, scripts, raw
   results, probe, backend selection, and direct optional hashbrown dependency.
   Retained Moka and the production lifetime/protocol regression cases.
2. Flattened RuntimeConfig into typed RTD, handle, and worker policies. Removed
   RtdConfig, HandleConfig, and AsyncRuntimeConfig; builders now operate on
   RuntimeConfig. The worker ceiling is the actual 64-bit wakeup-mask width,
   replacing the unexplained 32-worker restriction.
3. Shared only NonZeroU64 representation/constructor boilerplate through a
   private macro. Identity domains, promotion, and increments remain separate.
4. Removed 24 Runtime forwarding/duplicate methods and two obsolete lifecycle
   helpers. Tests use existing OpenDeps/ShutdownDeps capabilities.
5. Shared async payload, subscription connection, handle publication, and COM
   dispatch fixtures without removing intermediate assertions. Miri exposed a
   pre-existing stale pointer after moving an owned async payload. The responder
   now refreshes its pointer immediately before normal or fallback FFI delivery;
   its raw storage is private. The deep-copy regression passes Stacked Borrows
   and Tree Borrows and is included in the existing miri_ selection.
6. Removed 23 unused xlfn roots and one xlfn-package root from generator inputs.
   Regeneration reduced xlfn bindings from 26,151 to 14,067 bytes and package
   bindings from 7,907 to 6,316 bytes (13,675 bytes total); xlfn-sys is unchanged.
   Removed the now-obsolete TLIBATTR Default patch. No generated file was edited
   manually. Regenerating again produces the same output.
7. Audited fixed restrictions as detailed below; removed the borrowed reference
   table's arbitrary 1,024-area ceiling. Its u16 ABI count already bounds traversal
   to 65,535 entries without allocation. Empty tables and invalid coordinates
   remain rejected; a regression covers 1,025 valid areas and zero areas.

8. Removed the single-variant HandleTopicKey enum in favor of its existing
   FormulaRevisionKey domain type, the RuntimeExecutors container in favor of
   directly owning AsyncManager, and TaskShard in favor of its existing mutex.
   Field order, lock scopes, task-control destruction, formula key formatting,
   and wire serialization remain unchanged.

## Wrapper/delegation review

Searched named and tuple structs, small forwarding bodies, and enum declarations
across xlfn, xlfn-kernel, xlfn-common, xlfn-sys, xlfn-macros, xlfn-package and
cargo-xlfn. Reviewed non-generated candidates and their users, including cfg
branches. Removed candidates introduce no replacement trait or alias.

| Candidate family | Decision |
| --- | --- |
| HandleTopicKey, RuntimeExecutors, TaskShard | Removed in stage 8: no separate identity, lifetime or ownership protocol. Existing key/manager/mutex types express the same domain. |
| ResidentIndex, CacheGeneration, pointer/pin wrappers | Retain residency notification, epoch advancement, non-owning pointer contracts and reclamation boundaries. |
| HandleStore, HandleReadDomain, NewObject/ExistingObject, VerifiedHandleToken | Retain object ownership, admitted read domains, publication policy distinctions and authenticated token evidence. |
| OperationGate, DrainPermit, QuotaPermit, PublishedOwner, RetiredService, GenerationIndex | Retain public kernel domains, affine guards, reclamation obligations and bounded generation identity. |
| OpenDeps/ShutdownDeps, LifecycleControl, RuntimeObserver, GenerationServiceInputs | Retain operation-specific authority, observed transitions and affine transfer at generation commit/rollback. |
| OpenRollbackOutcome and shutdown/quiescence certificates | Retain restricted construction of terminal results/proofs; the private field prevents callers from constructing a successful outcome directly. |
| ExcelHost, RegistrationHost, RtdOpenContext, CallContext, MacroRuntime | Retain callback lifetime/capability boundaries, registration ABI policy and generated-code API boundary. |
| PreparedRegistrationSet, PreparedExcelString, ExcelNameKey, macros::UdfId | Retain completed validation, normalized identity and registration transaction domains. |
| Formula/read leases, COM references, module/registry/file leases, callback and return guards | Retain Drop behavior, OS/ABI ownership, typed Send/Sync contracts or thread affinity. |
| SheetId, CallId, CalculationId, nonzero IDs, HandleBindingLimit, AsyncWorkerCount | Retain domain separation and validated ranges. |
| Row, Column, BoundedVarArgs, ExcelErrorValue | Retain public conversion semantics and type-directed orientation/bounds/error behavior. |
| ScopedPerLookupCacheBenchmark, ScopedBatchCacheBenchmark | Retain distinct benchmark workloads; constructors select different read-scope lifetimes. |
| cargo-xlfn::Cli, package::CommitOutcome, DigestWriter, IoErrorSource | Retain parser shape, returned cleanup outcome, digest Write implementation or error-source integration. |
| Test Drop probes and mock wrappers | Retain deliberate destructor, callback and trait behavior needed by their regression cases. |

The other reviewed enums represent actual state/protocol choices or platform/test
implementations; no remaining experiment-only single-variant dispatch was found.

## Fixed-value inventory

Scope: all crates, excluding generated Win32 declarations. Searched MAX_*,
DEFAULT_*, *_COUNT, *_CAPACITY, *_SHARDS, *_LIMIT, and related MAX/DEFAULT and
plural benchmark names. A = protocol/correctness, B = bounded resource safety,
C = internal performance policy, D = unsupported restriction.

| Constants / location | Class | Decision and reason |
| --- | --- | --- |
| MAX_EXCEL_FUNCTION_ARGUMENTS, EXCEL_STRING_LIMIT (common) | A | Retain Excel call/string representation limits. |
| EXCEL_MAX_ROW/COLUMN and EXCEL_MAX_ROWS/COLUMNS (reference, value, raw) | A | Retain valid worksheet coordinate bounds. |
| MAX_REGISTER_ARGUMENT_HELP_ENTRIES (registration/schema) | A | Retain the registration call's remaining argument slots. |
| MAX_RTD_TOPIC_PARTS (subscription/topic, Windows automation) | A | Retain the RTD call's remaining argument slots in both feature-independent layers. |
| EXACT_LIMIT (value, subscription/value) | A | Retain exact integer-to-f64 representation checks. |
| AsyncWorkerCount::MAX (addin) | A | Derive from u64::BITS; one wakeup bit per worker is required. |
| ACTIVE_COUNT_MASK (kernel/sealable_counter) | A | Packed counter reserves a waiting bit. |
| NUMERIC_KEY_CAPACITY (handle/formula) | C | Initial allocation estimate from maximum serialized numeric widths; no rejection. |
| DEFAULT_STRIPE_COUNT, INGRESS_STRIPE_COUNT, RETURN_STRIPE_COUNT | C | Keep internal admission/return contention tuning. |
| HANDLE_PREPARE_STRIPE_COUNT, MIN_PUBLISHED_TOPIC_SHARDS, TASK_SHARDS, TOPIC_SHARDS | C | Keep internal lock/publication sharding. |
| MAINTENANCE_INTERVAL, RECLAIM_BACKPRESSURE_NODES (cache) | C | Keep maintenance cadence and the threshold that starts blocking reclamation; these do not reject user inputs. |
| Cache weight-budget clamp to u32::MAX | A | Matches the resident index weigher representation. |
| INLINE_UTF16_CAPACITY | C | Inline storage threshold, spilling to heap remains supported. |
| AsyncWorkerCount::DEFAULT | C | Four-worker default; no new configuration surface. |
| THREAD_COUNTS, WORKER_COUNTS, PRODUCER_COUNTS, HANDLE_COUNTS, CONCURRENT_THREAD_COUNTS, DEFAULT_OPERATIONS_PER_WORKER (benches) | C | Measurement workloads, not runtime restrictions. |
| HandleBindingLimit::DEFAULT, MAX_SUPPORTED_BINDINGS | B | Bound a generation's dense publication table. Fallible allocation alone would allow a huge successful allocation; retain the existing memory policy. |
| MAX_ARRAY_ELEMENTS, MAX_ARRAY_BYTES, MAX_RETURN_BYTES | B | Bound conversion work, owned copies and return arenas, with lower budgets on 32-bit hosts. Arithmetic checks do not bound total resource use; retain. |
| MAX_PENDING, MAX_ASYNC_HANDLE_BYTES (async) | B | Bound queued tasks and copied opaque host handles. Successful allocation is not a substitute for a queue/payload budget. |
| MAX_RTD_TOPIC_BYTES, DEFAULT_MAX_RTD_PENDING/ACTIVE/QUEUED_UPDATES/SOURCE_IDS/TOTAL_TOPIC_BYTES | B | Bound per-topic and aggregate retained subscription state. Keep existing typed RtdCapacity policies; allocation checks alone cannot enforce cumulative quotas. |
| MAX_PREPARED_ENTRIES, MAX_PREPARED_BYTES (package/error) | B | Bound an untrusted distribution's aggregate staged files/bytes. Keep traversal and staging budgets. |
| DIAGNOSTIC_QUEUE_CAPACITY, LOG_MAX_BYTES, DIAGNOSTIC_TEXT_MAX_BYTES | B | Bound background backlog, rotated log size and formatted diagnostic payloads. |
| CALLBACK_CLEANUP_AUDIT_CAPACITY | B | Bounded retained cleanup audit; allocation success would not limit accumulated observations. |
| MAX_TRACE_EVENTS (shutdown_trace, composition_refinement, handle/refinement) | B | Bound diagnostic/refinement history, including long-running test traces. |
| MAX_REFERENCE_AREAS (reference) | D | Removed. Borrowed table needs no allocation and already has a u16 ABI bound. |
| Former literal worker ceiling 32 | D | Removed in stage 2; actual 64-bit invariant retained. |

B limits were reviewed for replacement with typed or fallible allocation checks.
They enforce retained/aggregate memory or work budgets beyond allocation validity,
so removing them would change resource safety. No additional arbitrary production
limit was found in the surveyed families.

## Verification and limits

Stages 1–8 passed the four requested checks; stage 5/6 full suites each reported
787 passed and 7 ignored. Stage 7 reported 788 passed and 7 ignored and its
new reference-count regression passed Miri. Stage 6 also passed Windows MSVC 32/64-bit workspace
library Clippy with all features and blake3/pure. Earlier dependency-tree checks
confirmed that xlfn no longer directly depends on hashbrown for a resident backend.

Windows all-target checks were attempted but stopped in the alloca dev dependency
because this macOS environment lacks the Windows SDK malloc.h. Consequently,
Windows-only test fixtures were reviewed but not compiled or executed here.
Production libraries compile for both Windows targets. Real Excel/COM execution
still requires Windows; the seven explicitly ignored tests remain ignored.

During stage 7 validation, the cache clear-race test reported one pending node
once while other build/Miri jobs were running. Its immediate physical-reclamation
assertion did not reproduce in 50 consecutive isolated runs. Moka maintenance
passes are time-bounded; no cache ordering or assertion was changed to conceal
this observation. Keep this intermittent failure visible when interpreting the
passing final run.

Final stage validation: 788 tests passed, 7 ignored; workspace all-target/all-feature
Clippy, formatting and whitespace checks passed. Twenty all-target xlfn feature
configurations passed (all subsets of async/handles/rtd/refinement plus cache,
output, benchmark and combined configurations). Both Windows MSVC library
checks passed again. Guide validation passed all 29 chapters; the panic-boundary
checker accepted all 44 reviewed references.

## Commit sequence

1. a11b622 — resident-index experiment removal
2. f580ea9 — configuration flattening
3. 3c9f5ed — shared typed-ID boilerplate
4. eb4f1a4 — existing lifecycle capabilities
5. a13f93e — semantic fixtures and async payload pointer repair
6. 0dc64bd — binding root pruning
7. 9e2b768 — fixed-limit audit and reference bound removal
8. This review's final commit — wrapper/delegation cleanup
