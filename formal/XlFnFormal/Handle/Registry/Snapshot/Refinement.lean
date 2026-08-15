import XlFnFormal.Handle.Registry.Snapshot.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

inductive LineageKind where
  | fast (readerId : Nat)
  | slow (token : Token)
deriving DecidableEq, Repr

structure LeaseLineage where
  id : Nat
  kind : LineageKind
  cloneCount : Nat
deriving DecidableEq, Repr

structure ConcreteState where
  snapshot : State
  lineages : List LeaseLineage
deriving DecidableEq, Repr

def ConcreteState.findLineage? (c : ConcreteState) (id : Nat) : Option LeaseLineage :=
  c.lineages.find? (fun l => l.id == id)

def ConcreteState.findFastLineage? (c : ConcreteState) (readerId : Nat) : Option LeaseLineage :=
  c.lineages.find? (fun l => l.kind == .fast readerId)

def sumCloneCounts : List LeaseLineage → Nat
  | [] => 0
  | l :: ls => l.cloneCount + sumCloneCounts ls

def ConcreteState.totalPhysicalLeases (c : ConcreteState) : Nat :=
  sumCloneCounts c.lineages

def ConcreteState.updateLineageCloneCount
    (c : ConcreteState) (id : Nat) (newCount : Nat) : List LeaseLineage :=
  c.lineages.map (fun l => if l.id = id then { l with cloneCount := newCount } else l)

def ConcreteState.removeLineage (c : ConcreteState) (id : Nat) : List LeaseLineage :=
  c.lineages.filter (fun l => l.id != id)

def ConcreteState.LineagesUnique (c : ConcreteState) : Prop :=
  c.lineages.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def ConcreteState.LineagesPositive (c : ConcreteState) : Prop :=
  ∀ l ∈ c.lineages, l.cloneCount > 0

def ConcreteState.LineagesSound (c : ConcreteState) : Prop :=
  ∀ l ∈ c.lineages,
    match l.kind with
    | .fast readerId =>
        ∃ lookup ∈ c.snapshot.fastLookups, lookup.id = readerId ∧ lookup.stage = .validated
    | .slow _ => True

def ConcreteState.LineageAccounting (c : ConcreteState) : Prop :=
  c.lineages.length = c.snapshot.registry.activeLeases ∧
  c.snapshot.validatedFastLookups.length ≤ c.lineages.length

def ConcreteState.Invariant (c : ConcreteState) : Prop :=
  c.snapshot.Invariant ∧
  c.LineagesUnique ∧
  c.LineagesPositive ∧
  c.LineagesSound ∧
  c.LineageAccounting

theorem sumCloneCounts_nil : sumCloneCounts [] = 0 := rfl

theorem sumCloneCounts_pos_of_mem_pos
    {l : LeaseLineage} {ls : List LeaseLineage}
    (hMem : l ∈ ls) (hPos : l.cloneCount > 0) :
    sumCloneCounts ls > 0 := by
  induction ls with
  | nil => contradiction
  | cons head tail ih =>
      dsimp [sumCloneCounts]
      cases List.mem_cons.mp hMem with
      | inl hEq =>
          subst hEq
          omega
      | inr hTail =>
          have ihRes := ih hTail
          omega

theorem sumCloneCounts_eq_zero_iff_nil
    {ls : List LeaseLineage}
    (hPos : ∀ l ∈ ls, l.cloneCount > 0) :
    sumCloneCounts ls = 0 ↔ ls = [] := by
  refine ⟨?_, ?_⟩
  · intro hZero
    cases ls with
    | nil => rfl
    | cons head tail =>
        dsimp [sumCloneCounts] at hZero
        have hHeadPos := hPos head (List.mem_cons_self _ _)
        omega
  · intro hNil
    subst hNil
    rfl

theorem sumCloneCounts_ge_length
    {ls : List LeaseLineage}
    (hPos : ∀ l ∈ ls, l.cloneCount > 0) :
    sumCloneCounts ls ≥ ls.length := by
  induction ls with
  | nil =>
      dsimp [sumCloneCounts]
      exact Nat.le_refl 0
  | cons head tail ih =>
      dsimp [sumCloneCounts]
      simp only [List.length_cons]
      have hHeadPos := hPos head (List.mem_cons_self _ _)
      have hTailPos : ∀ l ∈ tail, l.cloneCount > 0 := by
        intro l hl
        exact hPos l (List.mem_cons_of_mem head hl)
      have ihRes := ih hTailPos
      omega

theorem zero_lineages_iff_zero_physical_leases
    {c : ConcreteState}
    (hPos : c.LineagesPositive) :
    c.lineages = [] ↔ c.totalPhysicalLeases = 0 := by
  dsimp [ConcreteState.totalPhysicalLeases]
  exact (sumCloneCounts_eq_zero_iff_nil hPos).symm

theorem active_lineages_le_physical_leases
    {c : ConcreteState}
    (hPos : c.LineagesPositive) :
    c.lineages.length ≤ c.totalPhysicalLeases := by
  dsimp [ConcreteState.totalPhysicalLeases]
  exact sumCloneCounts_ge_length hPos

inductive ConcreteEvent where
  | baseStep (e : Event)
  | cloneLineage (id : Nat)
  | dropCloneNonFinal (id : Nat)
  | dropCloneFinalFast (readerId : Nat) (lineageId : Nat)
  | dropCloneFinalSlow (lineageId : Nat)
deriving DecidableEq, Repr

inductive ConcreteStep : ConcreteState → ConcreteEvent → ConcreteState → Prop where
  | baseStep
      {c : ConcreteState} {e : Event} {s' : State}
      (hStep : Step c.snapshot e s')
      (hNoLineageChange :
        e ≠ .validateFastLookup e.readerId? ∧
        e ≠ .completeFastLookup e.readerId? ∧
        e ≠ .fallbackFastLookup e.readerId? ∧
        e ≠ .beginSlowLookup e.token? ∧
        e ≠ .endSlowLookup) :
      ConcreteStep c (.baseStep e) { c with snapshot := s' }

  | validateFastLookupLineage
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      (hNoLineage : c.findLineage? lineageId = none)
      (hStep : Step c.snapshot (.validateFastLookup readerId) s') :
      ConcreteStep c (.baseStep (.validateFastLookup readerId))
        { snapshot := s'
          lineages := c.lineages ++ [{ id := lineageId, kind := .fast readerId, cloneCount := 1 }] }

  | beginSlowLookupLineage
      {c : ConcreteState} {s' : State} {token : Token} {lineageId : Nat}
      (hNoLineage : c.findLineage? lineageId = none)
      (hStep : Step c.snapshot (.beginSlowLookup token) s') :
      ConcreteStep c (.baseStep (.beginSlowLookup token))
        { snapshot := s'
          lineages := c.lineages ++ [{ id := lineageId, kind := .slow token, cloneCount := 1 }] }

  | cloneLineage
      {c : ConcreteState} {id : Nat} {lineage : LeaseLineage}
      (hFind : c.findLineage? id = some lineage) :
      ConcreteStep c (.cloneLineage id)
        { c with lineages := c.updateLineageCloneCount id (lineage.cloneCount + 1) }

  | dropCloneNonFinal
      {c : ConcreteState} {id : Nat} {lineage : LeaseLineage}
      (hFind : c.findLineage? id = some lineage)
      (hGtOne : lineage.cloneCount > 1) :
      ConcreteStep c (.dropCloneNonFinal id)
        { c with lineages := c.updateLineageCloneCount id (lineage.cloneCount - 1) }

  | dropCloneFinalFast
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat} {lineage : LeaseLineage}
      (hFind : c.findLineage? lineageId = some lineage)
      (hKind : lineage.kind = .fast readerId)
      (hOne : lineage.cloneCount = 1)
      (hStep : Step c.snapshot (.completeFastLookup readerId) s') :
      ConcreteStep c (.dropCloneFinalFast readerId lineageId)
        { snapshot := s'
          lineages := c.removeLineage lineageId }

  | dropCloneFinalSlow
      {c : ConcreteState} {s' : State} {token : Token} {lineageId : Nat} {lineage : LeaseLineage}
      (hFind : c.findLineage? lineageId = some lineage)
      (hKind : lineage.kind = .slow token)
      (hOne : lineage.cloneCount = 1)
      (hStep : Step c.snapshot .endSlowLookup s') :
      ConcreteStep c (.dropCloneFinalSlow lineageId)
        { snapshot := s'
          lineages := c.removeLineage lineageId }

theorem clone_is_abstract_stutter
    {c c' : ConcreteState} {id : Nat}
    (hStep : ConcreteStep c (.cloneLineage id) c') :
    c'.snapshot = c.snapshot := by
  cases hStep
  rfl

theorem drop_clone_non_final_is_abstract_stutter
    {c c' : ConcreteState} {id : Nat}
    (hStep : ConcreteStep c (.dropCloneNonFinal id) c') :
    c'.snapshot = c.snapshot := by
  cases hStep
  rfl

theorem terminal_quiescence_equivalence
    {c : ConcreteState}
    (hInv : c.Invariant)
    (hPhysicalZero : c.totalPhysicalLeases = 0) :
    c.snapshot.registry.activeLeases = 0 ∧
    c.snapshot.validatedFastLookups = [] ∧
    c.lineages = [] := by
  have hLineagesNil := (zero_lineages_iff_zero_physical_leases hInv.2.2.1).mpr hPhysicalZero
  have hAcc := hInv.2.2.2.2
  rw [hLineagesNil] at hAcc
  dsimp at hAcc
  have hAct : c.snapshot.registry.activeLeases = 0 := hAcc.1.symm
  have hValLen : c.snapshot.validatedFastLookups.length = 0 := by
    have hLe := hAcc.2
    omega
  have hValNil : c.snapshot.validatedFastLookups = [] :=
    List.length_eq_zero_iff.mp hValLen
  exact ⟨hAct, hValNil, hLineagesNil⟩

end XlFnFormal.Handle.Registry.Snapshot
