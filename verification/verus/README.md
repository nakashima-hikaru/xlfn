# Verus Formal Verification in xlfn

This directory contains formal verification artifacts, specifications, and proofs powered by [Verus](https://verus-lang.github.io/verus/), a tool for verifying the formal correctness of Rust code using SMT (Z3).

---

## 1. Architectural Role & Responsibilities

xlfn enforces temporal ownership and reclamation guarantees through a multi-tier verification stack:

$$\boxed{\text{Lean 4 = Protocol Correctness}}$$
$$\boxed{\text{Verus = Rust Implementation Refinement}}$$
$$\boxed{\text{Loom = Memory-Ordering Exhaustive Schedule}}$$
$$\boxed{\text{Miri = Residual UB / Stacked & Tree Borrows}}$$

| Tool                                             | Target Scope                       | Key Verification Goals                                                       |
| :----------------------------------------------- | :--------------------------------- | :--------------------------------------------------------------------------- |
| **Lean 4** (`formal/XlFnFormal/`)                | Protocol & Lifecycle transitions   | Whole-system quiescence, shutdown certificate, generation invariants         |
| **Verus** (`verification/verus/`, `xlfn-kernel`) | Concurrency primitives & ownership | Bit arithmetic, atomic state transitions, tracked capabilities, raw pointers |
| **Loom** (`cargo test --loom`)                   | C++11 memory-order regressions     | Exhaustive interleavings of Relaxed/Acquire/Release/AcqRel                   |
| **Miri** (`just miri`)                           | Operational semantics              | Stacked Borrows / Tree Borrows, no aliasing violations or memory leaks       |

---

## 2. Invariant ID Traceability

Lean 4 theorems, Verus verified properties, and ordinary Rust safety comments share unified traceability tags:

```
                  [TR-RECLAIM-1]
                 /      |       \
                /       |        \
         Lean 4       Verus        Rust
     (Safety.lean) (kernel proof) (SAFETY: ...)
```

---

## 3. Current Verification Metrics & Results

| Crate / Target             | Verified Proofs  | Errors       | Assumes       | Core Guarantee                                                    |
| :------------------------- | :--------------- | :----------- | :------------ | :---------------------------------------------------------------- |
| **`sealable_counter`**     | 29               | 0            | 0             | SC-1..9: bitmask validity, carry-out isolation, waiter retention  |
| **`drain_gate`**           | 11               | 0            | 0             | DG-1..5: tracked permits, quiescence, final release exclusion     |
| **`published_owner`**      | 6                | 0            | 0             | PO-1..6: linear ownership, move invariance, single drop           |
| **`operation_gate`**       | 9                | 0            | 0             | OG-1..5: admission gate soundness, reclamation barrier            |
| **`rotating_read_domain`** | 12               | 0            | 0             | RRD-D1..D5: 2-gen rotation, seal-before-publish, no race          |
| **`service_slot`**         | 15               | 0            | 0             | SS-1..5: lazy publication, withdraw before drain, Box recovery    |
| **`cache_lease`**          | 17               | 0            | 0             | TR-\*: pin safety, observation soundness, absence of UAF          |
| **`handle_domain`**        | 13               | 0            | 0             | HD-1..5: call-scoped domain admission, delayed binding retirement |
| **Total**                  | **112 verified** | **0 errors** | **0 assumes** | **Full kernel & concurrency subsystem verified**                  |

---

## 5. TCB & Quality Rules

1. **Zero Assumes**: `assume(...)` directives are strictly rejected in CI (`just verus-audit`).
2. **Controlled External Bodies**: Any `#[verifier::external_body]` must be registered in `tools/verus_external_allowlist.json`.
3. **No Verification Twins Long-Term**: PoC duplicate implementations are transitional. Final proofs will verify production code directly.
4. **Zero Fast-Path Overhead**: All ghost specifications and proofs are completely erased at compilation time.

---

## 5. Usage

```bash
# Run Verus formal verification
just verus

# Audit verification codebase for assume / unapproved external_body violations
just verus-audit
```
