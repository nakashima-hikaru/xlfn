# Native representation connection: required work

This is an implementation plan, not a proof or a completion claim. The shared
transition/control kernels are useful, but the following concrete differences
prevent treating the current resource proofs as native implementation refinement.
The selected policy is to add no project-owned trusted adapters. Unsupported
native primitive connections remain incomplete; diagnostic/test evidence does not
close them. Standard-library semantics are already in TCB.md; asserting an entire Cache or
drain operation correct would exceed that primitive boundary.

## Current mismatches

| Native surface | Current proof surface | Missing connection |
| --- | --- | --- |
| `CacheNode.pins: AtomicUsize` in `cache.rs` | `inline_value::Allocation.pins` is a std atomic, but `queued_atomic::Node::new` creates a separate `Pins` with a fresh vstd atomic | Pin tokens must describe the allocation's actual pin field, not a second counter |
| Native CacheNode has no per-node mutex | The proof ledger now lives in an erased `AtomicInvariant`; observation, retirement and recovery operations no longer acquire a runtime node lock | The ghost updates still need to be located at the matching native index, admission, pin and queue events |
| A lookup admission keeps an observed allocation alive while accessing its metadata | `observed_generation` / `observed_resident` now borrow directly from the reader-owned Ticket | Implemented for metadata reads; native field identity and the remaining bookkeeping synchronization are still open |
| `SealableCounter.state: AtomicUsize` | `drain_gate::atomic_counter` creates an independent vstd atomic and instance | The permission and lifecycle instance must refer to the actual state field; retaining the word-width kernel alone is insufficient |
| `HandleReadDomain.{debt, queued}: AtomicUsize` | `counter_refinement` proves checked arithmetic against the corrected destruction-in-flight model; `completion` holds a separate finite-width debt value | The exact native debt and queued locations must be tied to retirement registration, certified queue withdrawal and post-destructor discharge across concurrent reclaimers; a fresh vstd counter would not identify either field |
| Native idle rotation uses the transition mutex, current atomic and retirement queue | The resource-backed `IdleHandoff` keeps a vstd transition WriteHandle and zero-count stripe leases through vstd publication, prepared-queue detachment and Cache/Handle heap-permission recovery; the verified callback runs before sealed controls are restored. Production `RotatingRetirementDomain` now owns the admission domain and both queues, selecting those same fields for registration, publication barriers and certified withdrawal | The production owner is not identified with the vstd transition/queue objects; native atomic and guard semantics, callback-to-native-callsite identity, destructor completion and weak-memory ordering remain unproved |
| `parking_lot` wait/notify with RAII guards | Executable wait models and vstd locks | Guard identity, unlock/relock, notification discipline and unwind exits must connect to the actual objects |
| `Box::leak` / `Box::from_raw` in Cache | `HeapPermission` is supplied to initialization and returned by recovery | Allocation identity and exact allocator permission must cross the native APIs once, without moving the inline payload during destruction |

## Reader-owned observation implementation

`Ticket` now owns both a receipt and the linear Cache observation. Its type
invariant binds the observation's memory to the receipt. Explicit node-instance
preconditions are preserved through Node and Snapshot APIs. The ledger retains
only admission shares and receipt bookkeeping; its share count still equals the
Cache observing count. Ending an observation consumes the receipt and observation,
removes the share, and decrements the count. Frozen coverage and zero-after-drain
proofs continue to use the retained shares.

Metadata reads and pin CAS borrow the Ticket observation directly, without taking
the ledger lock. The resident-load borrow gate now rejects consuming that Ticket
while the allocation reference remains live. This is a resource-protocol change,
not a new trusted primitive specification.

The ledger now uses the existing vstd `AtomicInvariant`, which is ghost state and
erases from executable code. The pin decrement still precedes final retirement
bookkeeping. Two invariant namespaces are kept distinct during recovery. The
remaining work is to connect each ghost update to the corresponding native
index, admission, pin or queue operation, using the same allocation and gate
instances throughout. This invariant removes extra executable node locking; it
does not identify the separate vstd pin counter with `CacheNode.pins`.

## Requirements at the currently unsupported primitive boundary

The pinned Verus 0.2026.09.13.671956e can type-check a direct native std
`AtomicUsize::load`, but does not prove even a freshly constructed atomic's
initial value. It rejects direct `std::sync::Mutex` and `Condvar` use at their
types, and rejects native `Box::into_raw`/`from_raw`. The reproducible probes are
`tools/probe_verus_atomic_contracts.py`, `tools/probe_verus_sync_contracts.py`,
and `tools/probe_verus_heap_contracts.py`. The std lock probe is diagnostic only:
production uses `parking_lot`, whose native object/guard identity is a separate
open obligation. A compiler rejection is never counted as a verified property.

These are unresolved obligations, not authorization to add trusted adapters.
Under the selected policy they must remain open until they can be established
without enlarging the project trusted base. Existing library contracts alone
do not establish correspondence to a separately allocated native field.

- Atomic construction issues exactly one permission for the actual field. All
  operations use that field, preserve its identity, and account for failed and
  spuriously failed CAS. Do not manufacture an identity relation between two
  freshly constructed counters.
- Per-location arithmetic/linearization does not establish cross-location
  publication. The actual Relaxed/Release/Acquire operations and fence obligations
  need a separate memory-order argument; silently substituting SeqCst is invalid.
- Atomic interior mutability must remain compatible with payload read permissions;
  a whole-node permission must not imply that mutable atomic contents are frozen.
- Mutex/Condvar contracts expose only primitive mutual exclusion and wait behavior.
  Admission, notification obligations, sealed quiescence and reclamation remain
  verified caller obligations, not trusted postconditions.
- Box conversion transfers matching initialized memory and allocator permission,
  including layout and provenance, exactly once. In-place destruction, unwind
  cleanup, generic Drop and unsized PublishedOwner cases require explicit treatment.
- Executable erasure must preserve native fields, operations and lock structure.
  A verified program with additional executable synchronization is not automatically
  a refinement of the lock-free native reader path.

### Handle debt location and completion interval

Production `HandleReadDomain::enqueue_reclaim` appends under the selected queue
lock, then increments `debt` and `queued`. `take_generation` and terminal close
subtract only `queued` when records leave their queues. `DrainedBindings::drop`
first destroys the detached records, then subtracts their count from `debt`
with Release ordering before taking the completion lock and notifying waiters.
The existing shared completion expression and checked counter kernels prove
the intended ordering and arithmetic in their executable models.

The remaining concurrent obligation is per-location and cross-location: each
retirement contributes exactly one debt unit to the native field, detachment
preserves that unit while destruction is in flight, and only the matching
completed batch discharges it. An independent vstd debt atomic would add another
verification twin unless its operations and linear receipts were connected to
the actual `HandleReadDomain` field and queue callbacks. Under the selected
no-new-trusted-adapter policy this is future native refinement work, not a
missing standalone counter lemma or authorization to assume native identity.

Completion requires the same allocation and gate instances through admission,
observation, pins, rotation, retirement and recovery. Neither this plan, diagnostic
support probes, nor additional independent models discharge that requirement.
