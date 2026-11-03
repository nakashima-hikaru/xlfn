# Verus trusted boundary

Verus checks the shared expressions and resource models in this directory.
Native Rust, Loom and Miri checks provide separate evidence. None establishes a
proof of the complete native binary, its dependencies or Excel. Lean and Verus
share invariant identifiers; there is no mechanically checked theorem importing
one prover's results into the other.

## Trusted primitives

| Dependency | Trusted contract | Caller obligation |
| --- | --- | --- |
| Target layout | `usize`/pointer widths and alignment on supported 32/64-bit targets | Use the matching representation; native Cache pins are `AtomicU32`, including on 64-bit hosts |
| Rust atomics | Indivisibility and the declared memory semantics of native atomic operations | Prove field identity, ordering, publication and ownership at the actual operations |
| `parking_lot::Mutex` | Mutual exclusion and guard release | Keep the correct guard through protected publication, detachment and callbacks |
| `parking_lot::Condvar` | Wait releases/reacquires the associated mutex; notifications wake eligible waiters and are not retained | Register/recheck under the mutex, tolerate spurious wakes and prevent lost wakeups |
| Allocation primitives | Valid uniquely owned allocation with matching size/alignment | Preserve initialization, layout, provenance and allocator rights through every handoff |
| vstd memory/resources | `PointsTo`, `Dealloc`, tokenized storage, locks, atomic invariants and their library contracts | Supply matching linear resources; a separately allocated proof object does not identify a native field |
| Compiler/verifier/solver | Rust compilation, Verus translation and Z3 reasoning | Preserve explicit project trust boundaries and inspect diagnostics |

Fairness and termination are not assumed by the safety proofs. Project audits
cover project source; they do not audit every expanded library implementation.

## Shared transitions and resource boundaries

### SealableCounter and DrainGate

The counter transition kernel is instantiated for both u32 and u64. Its named
invariants cover bounded active counts (`SC-1`), successful acquisition and sealed
rejection (`SC-2`, `SC-3`), underflow/final-release classification (`SC-4` through
`SC-6`), sealed-zero reopen (`SC-7`, `SC-8`), and retaining the final waiting
capability until notification is protected (`SC-9`, `SC-9b`). Overflow and
underflow use fail-stop transitions.

Production, Loom and Verus share DrainGate's release tail and wait-loop control.
The slow final release locks, releases, notifies if idle, then unlocks; no backend
access follows unlock. Waiting registers and reobserves after wakes and returns
only on an actual zero observation. Arbitrary sampled interference includes
spurious wakes; it supplies no termination theorem.

Resource invariants conserve instance-bound permits (`DG-1`, `DG-2`). Drain
completion needs an actual zero observation (`DG-3`, `DG-4`), and final release
retains the notification obligation (`DG-5`). Shared full-stripe scans, idle-seal
CAS and rollback preserve the modeled controllers and waiting flags. Stable
reclamation additionally requires sealing and exclusion of reopen/undo over the
relevant interval. Independent observations are not a simultaneous snapshot.

The vstd resource counters and stripe leases are separate from native
`SealableCounter.state`. Native location/history, stripe identity, guard/closure
wiring, RAII, unwind and wait/notify correspondence remain open.

### PublishedOwner, OperationGate and ServiceSlot

`HeapPermission<T>` combines initialized memory permission and allocator rights
with matching layout and provenance. Adoption requires matching ownership;
borrowing retains it; recovery consumes it. Zero-sized values require no
allocation. A pointer-address equality alone never creates memory permission.
The `PO-*` snapshot properties do not independently prove native Box recovery,
unsized payloads, Drop or raw-reader draining.

OperationGate models closed admission, drained active operations and linear
permit conservation (`OG-1` through `OG-5`). ServiceSlot models single owner,
valid publication, withdraw-before-drain, quiescence before extraction and
fault isolation (`SS-1` through `SS-5`). Native guard lifetimes, pointer
publication, Box and destruction still require representation connections.

### RotatingReadDomain and retirement queues

The `RRD-D*` invariants require admission through the current generation,
seal-before-publication, drain-before-recovery and permanent closure. Reader
select/acquire/retry, rotation, registration and certified withdrawal share
control expressions with production.

The vstd paths carry matching transition handles, controllers, zero-count leases
and queue-ready authority. Idle publication retains its exact leases and records
through Cache/Handle recovery. The callback returns matching heap permissions
and queue-ready identity before shared `finish_rotation!` restores sealed
controller state. Retry/cancellation retains the owned resources.

Native `RotatingRetirementDomain` owns the admission domain and both retirement
queues. Its certificate methods select those fields and reject a foreign owner
before locking or moving payloads. Ordinary extraction borrows the transition
state; terminal extraction takes both queues in the prescribed order. These
Rust ownership constraints and shared expressions do not identify native atomics,
mutexes, queue containers or callbacks with vstd objects. Native cross-location
ordering, stripe histories, destruction and terminal composition remain open.

### Cache observations, pins and recovery

Shared lookup control checks epoch/residency, acquires a pin, rechecks residency,
rolls back when necessary, ends the observation and hands off retirement. The
shared CAS retry loop preserves observed state on failure. Final native pin
release uses Release decrement followed by an Acquire fence only for the final
pin. Verus pin atomics use vstd's primitive contract; that does not establish the
native fence history. `just cache-release-ordering` separately checks removal
and late-placement mutations with Loom.

The resource model conserves creator/resident/flight/lease pins and observations.
Typed borrowing requires a live matching observation or lease; reclamation
requires the unique retirement ticket, zero pins and zero observations. Node
identity and immutable domain membership bind these resources. Reader-owned
`Ticket` retains its observation while metadata and pin CAS borrow directly
from it. The bookkeeping ledger uses an erased `AtomicInvariant`, with distinct
namespaces preserved during recovery.

Matching sealed-zero gate leases derive zero observations. Frozen coverage can
narrow before generation publication and recover a drained pending generation
while current readers remain active. Exact queue withdrawal preserves allocation
and owner agreement. Shared inline allocation fields and value projection
provide source correspondence for that projection.

The temporal invariants (`TR-OBSERVE-1`, `TR-LEASE-1`, `TR-ADMISSION-1`,
`TR-RECLAIM-1`, `TR-NO-UAF`) remain conditional on those ownership premises.
Actual native pin/admission fields, mixed widths, resident-index snapshots,
queue publication cutoff, ghost-event placement, Box recovery and in-place Drop
are not fully connected. A proof atomic allocated beside a native atomic does
not close that gap.

### Handle publication and destruction debt

Handle invariants reject admission after closure (`HD-1`), account for retirement
before reclamation (`HD-2`), require generation quiescence for detachment
(`HD-3`), retain debt throughout destruction (`HD-4`), and derive final zero debt
from completed destruction (`HD-5`). Detachment decreases queued counts while
debt includes both queued records and destruction in flight.

Resource-backed AtomicPtr observations retain gate admission and permission
through retirement. Matching drain resources exclude active observations before
recovering the original allocation. Queue locks, exact sequence/coverage bounds,
prepared authority and owner-bound batches preserve records through registration,
publication and recovery. Completion receipts are linear and batch-specific.

Native drained batches borrow their domain and discharge debt only after their
records have been destroyed. Shared merge checks reject foreign owners before
moving payloads. Native publication additionally requires the matching writer
guard; compile-fail checks reject early guard release and simultaneous writers.
The native pointer/debt/queued locations, weak-memory histories, raw allocation
identity and arbitrary Drop behavior remain separate obligations.

## Source audit policy

`just verus-audit` enforces these project rules:

- Reject `assume`, `admit`, `assume_specification`, axioms and termination assumptions.
- Audit qualified/imported/function-value references, grouped imports and aliases.
  Even an ordinary API using a reserved assumption name must be renamed.
- Register approved external bodies/specifications, trusted annotations, trait
  boundaries and equivalent internal attributes by exact repository-relative
  `path:line` in `tools/verus_external_allowlist.json`.
- Do not use basename-only approval or silently enlarge the trusted base to
  bridge unsupported native objects.

Method calls, ordinary declarations, comments and strings are permitted. The
audit is a source inventory, not complete Rust/Verus name resolution or a proof
that every possible macro expansion is free of trust boundaries. Semantic proof
checks and negative mutations remain necessary. Parser errors, unsupported
operations and timeouts never establish a verified property.

[REFINEMENT_WORKLIST.md](REFINEMENT_WORKLIST.md) tracks current completion gaps;
[NATIVE_ADAPTER_PLAN.md](NATIVE_ADAPTER_PLAN.md) states the required native work.
