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
| Pin acquisition and release have no per-node mutex | `acquire_observed` now borrows a reader-owned token without a lock; `observe`, `end_observation` and final-release completion still use `ledger: vstd::RwLock`; nonfinal release now returns without it | Proof-only executable locking changes allowed interleavings; the bookkeeping must become ghost resources attached to real linearization points |
| A lookup admission keeps an observed allocation alive while accessing its metadata | `observed_generation` / `observed_resident` now borrow directly from the reader-owned Ticket | Implemented for metadata reads; native field identity and the remaining bookkeeping synchronization are still open |
| `SealableCounter.state: AtomicUsize` | `drain_gate::atomic_counter` creates an independent vstd atomic and instance | The permission and lifecycle instance must refer to the actual state field; retaining the word-width kernel alone is insufficient |
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

The next synchronization gap is the write-side ledger: registration, observation completion,
final-release completion and recovery still acquire an executable lock absent
from the native node. The pin decrement now precedes this lock, so a nonfinal
release no longer serializes through it. Its ghost resources must be transferred at actual index/admission/pin/queue
linearization points. Moving that lock into a trusted body, or treating its
serialization as native behavior, would not satisfy the selected policy.

## Requirements at the currently unsupported primitive boundary

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

Completion requires the same allocation and gate instances through admission,
observation, pins, rotation, retirement and recovery. Neither this plan, diagnostic
support probes, nor additional independent models discharge that requirement.
