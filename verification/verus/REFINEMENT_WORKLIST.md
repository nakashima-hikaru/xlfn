# Production refinement worklist

The objective is the native unsafe ownership path. A row closes only when the
production connection, proof composition and relevant validation are present.
Model proofs and shared source expressions do not establish native field
identity or weak-memory correctness by themselves.

| Requirement | Production surface | Current connection | Remaining work |
| --- | --- | --- | --- |
| Checked admission/release and widths | `crates/xlfn-kernel/src/sealable_counter/transitions.rs` | Shared 32/64-bit transition expressions | Native atomic location and concurrent-history correspondence |
| Final release and wait observation | `drain_gate.rs`, `drain_gate/protocol.rs` | Shared lock/release/notify/unlock and reobservation control; vstd permits | Native guard identity, wait/notify, RAII and unwind composition |
| Striped drain and idle rollback | `drain_gate.rs` | Shared full-stripe scan, idle seal/rollback and resource-backed zero-count leases | Actual stripe identity, stable no-reopen interval and concurrent histories |
| Reader admission and generation reuse | `rotating_read_domain.rs` | Shared select/acquire/retry/permit control and generation resources | Native selected gate, allocation identity and interference composition |
| Rotation and pending recovery | `rotating_read_domain.rs` | Shared seal/publish/reopen and callback-before-restoration order; vstd arbitrary-stripe drivers | Native atomic/lock fields, queue cutoff and callback callsite identity |
| Publication barriers and terminal take | `rotating_read_domain.rs` (`RotatingRetirementDomain`) | One native owner selects admission domain and retirement queues; shared authorization and take expressions | Native primitive semantics and complete terminal drain histories |
| Cache pin/observation/reclaim | `crates/xlfn/src/cache.rs`, `cache/pin_transitions.rs` | Shared pin and lookup branches; heap-backed observation, pin, retirement and queue resources | Actual `AtomicU32` pin field, mixed-width admission, resident index, allocation and Box/Drop mapping |
| Handle observation/debt/reclaim | `handle/domain.rs`, `handle/binding.rs`, `handle/object.rs` | Shared registration, batch transfer, checked counters and destruction-completion tail; resource-backed retained reads | Native AtomicPtr/debt/queued fields, queue histories, allocation identity and Drop completion |
| Allocation ownership | `published_owner.rs` and callers | Initialized memory plus allocator permission retained through modeled recovery | Native Box/DST transfer, borrowing and in-place destruction |
| Cross-layer composition | Lean invariants, Verus and unsafe callsites | Corresponding invariant IDs, trace checkers and shared kernels | Mechanically checked native and cross-prover composition |

The policy is to add no project-owned trusted adapters to close these gaps.
Unsupported native connections remain incomplete. Primitive synchronization and
allocation contracts cannot substitute for verified caller ownership
preconditions.

The [native adapter plan](NATIVE_ADAPTER_PLAN.md) specifies the concrete work.
[TCB.md](TCB.md) records the trusted library boundary and claim limits. Proof
entry points and the mutation inventory are maintained in source rather than in
historical counts or local execution diaries.

## Ongoing validation

- `just verus-audit` rejects project-local assumptions and unapproved trust boundaries.
- `just verus` checks proof targets, mutation sensitivity and borrow rejection.
- `just cache-release-ordering` checks the native shared final-pin release ordering with Loom mutations.
- `just miri` and focused Miri recipes exercise maintained ownership paths.
- CI also checks Rust-generated traces against the Lean checkers and exercises
  Windows/feature configurations.

Local Rust, Loom, Miri and Verus results have distinct scopes. They do not
establish live Windows/Excel behavior, physical DLL unload, or a whole-program
native refinement theorem. Committed cleanup quarantine remains outside the
Composition checker's successful-return theorem.
