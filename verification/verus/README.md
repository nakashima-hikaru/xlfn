# Verus Formal Verification in xlfn

This directory contains formal verification artifacts, specifications, and proofs powered by [Verus](https://verus-lang.github.io/verus/), a tool for verifying the formal correctness of Rust code using SMT (Z3).

---

## 1. Architectural Role & Responsibilities

xlfn enforces temporal ownership and reclamation guarantees through a multi-tier verification stack:

$$\boxed{\text{Lean 4 = Protocol Correctness}}$$
$$\boxed{\text{Verus = Implementation-Oriented Protocol Verification}}$$
$$\boxed{\text{Loom = Memory-Ordering Exhaustive Schedule}}$$
$$\boxed{\text{Miri = Residual UB / Stacked & Tree Borrows}}$$

| Tool                                             | Target Scope                       | Key Verification Goals                                                       |
| :----------------------------------------------- | :--------------------------------- | :--------------------------------------------------------------------------- |
| **Lean 4** (`formal/XlFnFormal/`)                | Protocol & Lifecycle transitions   | Whole-system quiescence, shutdown certificate, generation invariants         |
| **Verus** (`verification/verus/`, `xlfn-kernel`) | Concurrency protocol & models      | Dual 32/64-bit arithmetic, state transitions, tracked tokens, raw pointers   |
| **Loom** (`cargo test --loom`)                   | C++11 memory-order regressions     | Exhaustive interleavings of Relaxed/Acquire/Release/AcqRel                   |
| **Miri** (`just miri`)                           | Operational semantics              | Stacked Borrows / Tree Borrows, no aliasing violations or memory leaks       |

---

## 2. Invariant ID Traceability & Refinement Architecture

Lean 4 theorems, Verus verified properties, and ordinary Rust safety comments share unified traceability tags:

```
                  [TR-RECLAIM-1]
                 /      |       \
                /       |        \
         Lean 4       Verus        Rust
     (Safety.lean) (kernel proof) (SAFETY: ...)
```

The verification stack bridges abstract protocol mathematics to production hardware:

```text
┌────────────────────────────────────────────────────────┐
│ Lean 4 (`formal/XlFnFormal/`)                         │
│ - Protocol correctness, generational rotation, drain   │
└───────────────────────────┬────────────────────────────┘
                            │ Shared Invariant IDs ([SC-*], [DG-*], [TR-*], etc.)
                            ▼
┌────────────────────────────────────────────────────────┐
│ Verus (`verification/verus/`)                          │
│ - Implementation-oriented protocol models & bit proofs │
│ - Dual 32-bit (i686) & 64-bit mathematical safety      │
│ - SSOT direct transitions verification                 │
└───────────────────────────┬────────────────────────────┘
                            │ Single Source of Truth Transitions
                            ▼
┌────────────────────────────────────────────────────────┐
│ Production Rust (`crates/xlfn-kernel/`, `crates/xlfn`) │
│ - Verified pure transition kernels                     │
│ - Atomic RMW loops, OS synchronization, allocations   │
└───────────────────────────┬────────────────────────────┘
               ┌────────────┴────────────┐
               ▼                         ▼
┌──────────────────────────┐ ┌──────────────────────────┐
│ Loom (`cargo test --loom`)│ │ Miri (`just miri`)       │
│ - C++11 memory orderings │ │ - Pointer provenance     │
│ - Exhaustive scheduling  │ │ - Aliasing / Tree Borrows│
└──────────────────────────┘ └──────────────────────────┘
```

---

## 3. Current Verification Metrics & Results

| Crate / Target             | Verified Proofs  | Errors       | Assumes       | Refinement Status & Guarantees                                    |
| :------------------------- | :--------------- | :----------- | :------------ | :---------------------------------------------------------------- |
| **`sealable_counter`**     | Verified (dual)  | 0            | 0             | **SSOT End-to-End**: SC-1..9b verified on shared code (32 & 64)   |
| **`drain_gate`**           | 11               | 0            | 0             | **Protocol Model**: DG-1..5 tracked permits, drain quiescence     |
| **`published_owner`**      | 6                | 0            | 0             | **Protocol Model**: PO-1..6 linear ownership, single drop         |
| **`operation_gate`**       | 9                | 0            | 0             | **Protocol Model**: OG-1..5 admission gate soundness, quiescence  |
| **`rotating_read_domain`** | 12               | 0            | 0             | **Protocol Model**: RRD-D1..D5 2-gen rotation, seal-before-pub    |
| **`service_slot`**         | 15               | 0            | 0             | **Protocol Model**: SS-1..5 lazy publication, Box extraction      |
| **`cache_lease`**          | 17               | 0            | 0             | **Protocol Model**: TR-\* pin safety, observation, absence of UAF |
| **`handle_domain`**        | 13               | 0            | 0             | **Protocol Model**: HD-1..5 call-scoped domain admission          |
| **Total**                  | **Verified**     | **0 errors** | **0 assumes** | **Kernel & concurrency protocol models verified; SC SSOT complete**|

---

## 4. Refinement Status & Roadmap

1. **Tier 1 — Single Source of Truth (SSOT)**:
   - `SealableCounter`: The production transition kernel (`crates/xlfn-kernel/src/sealable_counter/transitions.rs`) is directly verified by Verus for both 32-bit (`i686`) and 64-bit platforms with identical fail-stop semantics (`FailStop` on overflow/underflow). No duplicate verification twin exists.
2. **Tier 2 — Protocol Model Verification (Active Roadmap)**:
   - `DrainGate`, `PublishedOwner`, `OperationGate`, `RotatingReadDomain`, `ServiceSlot`, `CacheLease`, `HandleDomain`: Protocol models and ownership tokens verified in Verus with 0 errors and 0 assumes. SSOT direct refinement is being expanded progressively using the `SealableCounter` template.

---

## 5. TCB & Quality Rules

1. **Zero Assumes**: `assume(...)` directives are strictly rejected in CI (`just verus-audit`).
2. **Controlled External Bodies**: Any `#[verifier::external_body]` must be registered in `tools/verus_external_allowlist.json`.
3. **No Verification Twins Long-Term**: Verification twins are eliminated in favor of single-source-of-truth verified kernels.
4. **Zero Fast-Path Overhead**: All ghost specifications and proofs are completely erased at compilation time.

---

## 5. Usage

```bash
# Run Verus formal verification
just verus

# Audit verification codebase for assume / unapproved external_body violations
just verus-audit
```
