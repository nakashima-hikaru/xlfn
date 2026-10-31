# Native representation work

This plan lists missing native connections. Shared kernels and resource models
remain useful, but completion requires the same fields, allocations and gate
instances through admission, observation, pinning, rotation, retirement and
recovery. No project-owned trusted adapter is authorized to assume that result.

| Native surface | Proof surface | Required connection |
| --- | --- | --- |
| `CacheNode.pins: AtomicU32` | Shared allocation layout plus a separately constructed vstd pin atomic | Pin tokens must describe the actual field; account for 32-bit pins with `usize` admission counters |
| Cache observations and bookkeeping | Reader-owned `Ticket` and erased `AtomicInvariant` ledger | Place ghost updates at matching native resident-index, admission, pin and queue events |
| `SealableCounter.state: AtomicUsize` | Independent vstd atomic and lifecycle instance | Bind permissions and operations to the actual state field and history |
| `HandleReadDomain.{debt, queued}` | Shared checked counters and completion ordering with modeled debt resources | Connect enqueue, detachment, concurrent destruction and final debt discharge to the actual fields |
| Idle rotation and retirement queues | vstd transition handle, current atomic, stripe leases and exact withdrawn records | Connect the native owner, guards, callback and restored state to those resources |
| `parking_lot` guards and wait/notify | Executable wait models and vstd locks | Preserve object identity, unlock/relock, lost-wakeup discipline, RAII and unwind exits |
| Box conversion and Drop | `HeapPermission` supplied at initialization and recovered after drain | Transfer matching layout/provenance/allocator rights exactly once and destroy inline payloads in place |

## Required invariants

- Atomic construction issues permission for the actual location. Failed and
  spuriously failed CAS preserve the matching resources. A fresh proof atomic
  does not identify an existing native field.
- Per-location arithmetic does not establish cross-location publication. Native
  Relaxed/Release/Acquire operations and fences need a memory-order argument;
  a SeqCst proof backend cannot silently replace them.
- Atomic interior mutability remains compatible with payload read permissions.
  A whole-allocation permission cannot imply that embedded atomics are frozen.
- Primitive lock/wait contracts provide mutual exclusion and wait behavior.
  Admission, notification, sealed quiescence and reclamation remain caller
  obligations.
- Box conversion preserves allocation identity, initialized memory, provenance
  and allocator permission. Generic Drop, unwind cleanup and unsized
  `PublishedOwner` payloads require explicit treatment.
- Ghost erasure preserves native operations and lock structure. The current
  Cache ledger erases, but its separate proof pin atomic still needs the native
  field connection.

## Handle completion interval

`enqueue_reclaim` appends under the selected queue lock, then increments debt
and queued counts. Detachment subtracts queued counts while preserving debt.
`DrainedBindings::drop` destroys the detached records before discharging debt
with Release ordering and notifying waiters under the completion lock. The
shared completion expression models that order; native field histories and
matching batch identity remain obligations across concurrent reclaimers.

## Windows teardown boundary

COM export-drain preparation failure retains the affine `ModuleClosing`
capability in terminal quarantine without issuing `ExportsDrained` or retrying
cleanup. The DLL residency lease remains held. Rust regressions and the
Shutdown trace checker cover retention and the `rtdShutdownFailed` prefix.
They do not prove native COM setup, event/thread creation or concrete retained
owner identity. Committed cleanup quarantine is outside the Composition
checker's successful-return theorem.

Unsupported primitives must remain explicit open obligations. Compiler
rejection and standalone support probes do not prove properties. See
[TCB.md](TCB.md) for the trusted boundary and
[REFINEMENT_WORKLIST.md](REFINEMENT_WORKLIST.md) for the maintained status.
