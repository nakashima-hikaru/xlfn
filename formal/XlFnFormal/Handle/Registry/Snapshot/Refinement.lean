import XlFnFormal.Handle.Registry.Snapshot.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

inductive LineageKind where
  | tentativeFast (readerId : Nat)
  | validatedFast (readerId : Nat)
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
  c.lineages.find? (fun l =>
    match l.kind with
    | .tentativeFast id => id == readerId
    | .validatedFast id => id == readerId
    | .slow _ => false)

def sumCloneCounts : List LeaseLineage → Nat
  | [] => 0
  | l :: ls => l.cloneCount + sumCloneCounts ls

theorem sumCloneCounts_append
    {lhs rhs : List LeaseLineage} :
    sumCloneCounts (lhs ++ rhs) = sumCloneCounts lhs + sumCloneCounts rhs := by
  induction lhs with
  | nil => simp [sumCloneCounts]
  | cons head tail ih =>
      simp [sumCloneCounts, ih, Nat.add_assoc]

def ConcreteState.tentativeLineages (c : ConcreteState) : List LeaseLineage :=
  c.lineages.filter (fun l =>
    match l.kind with
    | .tentativeFast _ => true
    | .validatedFast _ => false
    | .slow _ => false)

def ConcreteState.validatedFastLineages (c : ConcreteState) : List LeaseLineage :=
  c.lineages.filter (fun l =>
    match l.kind with
    | .tentativeFast _ => false
    | .validatedFast _ => true
    | .slow _ => false)

def ConcreteState.slowLineages (c : ConcreteState) : List LeaseLineage :=
  c.lineages.filter (fun l =>
    match l.kind with
    | .tentativeFast _ => false
    | .validatedFast _ => false
    | .slow _ => true)

def ConcreteState.totalPhysicalLeases (c : ConcreteState) : Nat :=
  sumCloneCounts c.lineages

def ConcreteState.totalTentativePhysicalLeases (c : ConcreteState) : Nat :=
  sumCloneCounts c.tentativeLineages

def ConcreteState.totalCommittedPhysicalLeases (c : ConcreteState) : Nat :=
  sumCloneCounts c.validatedFastLineages + sumCloneCounts c.slowLineages

def ConcreteState.totalCommittedLineages (c : ConcreteState) : Nat :=
  c.validatedFastLineages.length + c.slowLineages.length

def ConcreteState.updateLineageCloneCount
    (c : ConcreteState) (id : Nat) (newCount : Nat) : List LeaseLineage :=
  c.lineages.map (fun l => if l.id = id then { l with cloneCount := newCount } else l)

def ConcreteState.updateLineageKind
    (c : ConcreteState) (id : Nat) (kind : LineageKind) : List LeaseLineage :=
  c.lineages.map (fun l => if l.id = id then { l with kind := kind } else l)

def ConcreteState.removeLineage (c : ConcreteState) (id : Nat) : List LeaseLineage :=
  c.lineages.filter (fun l => l.id != id)

def ConcreteState.LineagesUnique (c : ConcreteState) : Prop :=
  c.lineages.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def ConcreteState.LineagesPositive (c : ConcreteState) : Prop :=
  ∀ l ∈ c.lineages, l.cloneCount > 0

def ConcreteState.TentativeLineagesUnit (c : ConcreteState) : Prop :=
  ∀ l ∈ c.lineages, (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1

def ConcreteState.LineagesSound (c : ConcreteState) : Prop :=
  ∀ l ∈ c.lineages,
    match l.kind with
    | .tentativeFast readerId =>
        ∃ lookup ∈ c.snapshot.fastLookups,
          lookup.id = readerId ∧ lookup.stage = .tentative ∧ l.id = readerId
    | .validatedFast readerId =>
        ∃ lookup ∈ c.snapshot.fastLookups,
          lookup.id = readerId ∧ lookup.stage = .validated ∧ l.id = readerId
    | .slow token => token.session = c.snapshot.registry.session

def ConcreteState.LineageAccounting (c : ConcreteState) : Prop :=
  c.tentativeLineages.length = c.snapshot.tentativeFastLookups.length ∧
  c.validatedFastLineages.length = c.snapshot.validatedFastLookups.length ∧
  c.totalTentativePhysicalLeases = c.tentativeLineages.length ∧
  c.totalCommittedLineages = c.snapshot.registry.activeLeases

def ConcreteState.Invariant (c : ConcreteState) : Prop :=
  c.snapshot.Invariant ∧
  c.LineagesUnique ∧
  c.LineagesPositive ∧
  c.TentativeLineagesUnit ∧
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
        have hHeadPos := hPos head List.mem_cons_self
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
      have hHeadPos := hPos head List.mem_cons_self
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

theorem sumCloneCounts_eq_length_of_unit
    {ls : List LeaseLineage}
    (hUnit : ∀ l ∈ ls, l.cloneCount = 1) :
    sumCloneCounts ls = ls.length := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      dsimp [sumCloneCounts]
      have hHead := hUnit head List.mem_cons_self
      have hTail : ∀ l ∈ tail, l.cloneCount = 1 := by
        intro l hMem
        exact hUnit l (List.mem_cons_of_mem head hMem)
      rw [hHead, ih hTail]
      omega

theorem pairwise_updateLineageCloneCount
    {ls : List LeaseLineage} {id newCount : Nat}
    (hPair : ls.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)) :
    (ls.map (fun l => if l.id = id then { l with cloneCount := newCount } else l)).Pairwise
      (fun lhs rhs => lhs.id ≠ rhs.id) := by
  apply pairwise_map hPair
  intro a _ b _ hR
  split <;> split <;> exact hR

theorem pairwise_updateLineageKind
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    (hPair : ls.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)) :
    (ls.map (fun l => if l.id = id then { l with kind := kind } else l)).Pairwise
      (fun lhs rhs => lhs.id ≠ rhs.id) := by
  apply pairwise_map hPair
  intro a _ b _ hR
  split <;> split <;> exact hR

theorem positive_updateLineageCloneCount
    {ls : List LeaseLineage} {id newCount : Nat}
    (hPos : ∀ l ∈ ls, l.cloneCount > 0)
    (hNew : 0 < newCount) :
    ∀ l ∈ ls.map (fun x => if x.id = id then { x with cloneCount := newCount } else x),
      l.cloneCount > 0 := by
  intro l hMem
  rcases List.mem_map.mp hMem with ⟨orig, hOrig, rfl⟩
  split
  · exact hNew
  · exact hPos orig hOrig

theorem positive_updateLineageKind
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    (hPos : ∀ l ∈ ls, l.cloneCount > 0) :
    ∀ l ∈ ls.map (fun x => if x.id = id then { x with kind := kind } else x),
      l.cloneCount > 0 := by
  intro l hMem
  rcases List.mem_map.mp hMem with ⟨orig, hOrig, rfl⟩
  split <;> exact hPos orig hOrig

theorem unit_updateLineageKind
    {ls : List LeaseLineage} {id : Nat} {readerId : Nat}
    (hUnit : ∀ l ∈ ls, (∃ oldReaderId, l.kind = .tentativeFast oldReaderId) → l.cloneCount = 1) :
    ∀ l ∈ ls.map (fun x => if x.id = id then { x with kind := .validatedFast readerId } else x),
      (∃ oldReaderId, l.kind = .tentativeFast oldReaderId) → l.cloneCount = 1 := by
  intro l hMem hTentative
  rcases List.mem_map.mp hMem with ⟨orig, hOrig, rfl⟩
  by_cases hId : orig.id = id
  · simp [hId] at hTentative
  · simp [hId] at hTentative
    simpa [hId] using hUnit orig hOrig hTentative

theorem map_updateLineageKind_filter_length_eq
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    {p : LeaseLineage → Bool}
    (hP : ∀ l, p { l with kind := kind } = p l) :
    ((ls.map (fun l => if l.id = id then { l with kind := kind } else l)).filter p).length =
      (ls.filter p).length := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      by_cases hId : head.id = id
      · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
            { head with kind := kind } := by simp [hId]
        have hHeadP : p { head with kind := kind } = p head := hP head
        have hHeadP' : p { id := id, kind := kind, cloneCount := head.cloneCount } = p head := by
          simpa [hId] using hHeadP
        have hTail := ih
        cases hHeadValue : p head with
        | false =>
            simpa [List.map, List.filter, hId, hHeadMap, hHeadP', hHeadValue] using hTail
        | true =>
            simpa [List.map, List.filter, hId, hHeadMap, hHeadP', hHeadValue] using
              congrArg Nat.succ hTail
      · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
            head := by simp [hId]
        have hTail := ih
        cases hHeadValue : p head with
        | false =>
            simpa [List.map, List.filter, hId, hHeadMap, hHeadValue] using hTail
        | true =>
            simpa [List.map, List.filter, hId, hHeadMap, hHeadValue] using
              congrArg Nat.succ hTail

theorem map_updateLineageKind_filter_eq_of_target_removed
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    {p : LeaseLineage → Bool}
    (hTarget : ∀ l ∈ ls, l.id = id → p { l with kind := kind } = false) :
    (ls.map (fun l => if l.id = id then { l with kind := kind } else l)).filter p =
      (ls.filter p).filter (fun l => l.id != id) := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      by_cases hId : head.id = id
      · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
            { head with kind := kind } := by simp [hId]
        have hTargetHead : p { id := id, kind := kind, cloneCount := head.cloneCount } = false := by
          simpa [hId] using hTarget head List.mem_cons_self hId
        have hTailTarget : ∀ l ∈ tail, l.id = id →
            p { l with kind := kind } = false := by
          intro l hMem hLId
          exact hTarget l (List.mem_cons_of_mem head hMem) hLId
        have hTail := ih hTailTarget
        cases hHeadValue : p head with
        | false =>
            simpa [List.map, List.filter, List.filter_filter, Bool.and_comm,
              hId, hHeadMap, hTargetHead, hHeadValue] using hTail
        | true =>
            simpa [List.map, List.filter, List.filter_filter, Bool.and_comm,
              hId, hHeadMap, hTargetHead, hHeadValue] using hTail
      · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
            head := by simp [hId]
        have hTailTarget : ∀ l ∈ tail, l.id = id →
            p { l with kind := kind } = false := by
          intro l hMem hLId
          exact hTarget l (List.mem_cons_of_mem head hMem) hLId
        have hTail := ih hTailTarget
        cases hHeadValue : p head with
        | false =>
            simpa [List.map, List.filter, List.filter_filter, Bool.and_comm,
              hId, hHeadMap, hHeadValue] using hTail
        | true =>
            simpa [List.map, List.filter, List.filter_filter, Bool.and_comm,
              hId, hHeadMap, hHeadValue] using hTail

theorem map_updateLineageKind_eq_self_of_no_id
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    (hNoId : ∀ l ∈ ls, l.id ≠ id) :
    ls.map (fun l => if l.id = id then { l with kind := kind } else l) = ls := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      have hHead := hNoId head List.mem_cons_self
      have hTail : ∀ l ∈ tail, l.id ≠ id := by
        intro l hMem
        exact hNoId l (List.mem_cons_of_mem head hMem)
      have hHeadMap : (if head.id = id then { head with kind := kind } else head) = head := by
        simp [hHead]
      simp only [List.map]
      rw [hHeadMap, ih hTail]

theorem map_updateLineageKind_filter_length_of_target_added
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    {p : LeaseLineage → Bool}
    (hPair : ls.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hTarget : ∀ l ∈ ls, l.id = id → p { l with kind := kind } = true)
    (hOld : ∀ l ∈ ls, l.id = id → p l = false)
    (hMemTarget : ∃ l ∈ ls, l.id = id) :
    ((ls.map (fun l => if l.id = id then { l with kind := kind } else l)).filter p).length =
      (ls.filter p).length + 1 := by
  induction ls with
  | nil =>
      rcases hMemTarget with ⟨l, hMem, hId⟩
      contradiction
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTailPair =>
          by_cases hId : head.id = id
          · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
                { head with kind := kind } := by simp [hId]
            have hTargetHead : p { id := id, kind := kind, cloneCount := head.cloneCount } = true := by
              simpa [hId] using hTarget head List.mem_cons_self hId
            have hOldHead := hOld head List.mem_cons_self hId
            have hNoTail : ∀ l ∈ tail, l.id ≠ id := by
              intro l hMem hLId
              exact hHead l hMem (hId.trans hLId.symm)
            have hTailMap := map_updateLineageKind_eq_self_of_no_id
              (ls := tail) (id := id) (kind := kind) hNoTail
            simp [List.map, List.filter, hId, hHeadMap, hTargetHead, hOldHead, hTailMap]
          · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
                head := by simp [hId]
            have hTailTarget : ∀ l ∈ tail, l.id = id →
                p { l with kind := kind } = true := by
              intro l hMem hLId
              exact hTarget l (List.mem_cons_of_mem head hMem) hLId
            have hTailOld : ∀ l ∈ tail, l.id = id → p l = false := by
              intro l hMem hLId
              exact hOld l (List.mem_cons_of_mem head hMem) hLId
            have hTailMem : ∃ l ∈ tail, l.id = id := by
              rcases hMemTarget with ⟨l, hMem, hLId⟩
              rcases List.mem_cons.mp hMem with hEq | hTailMem
              · subst l
                exact (hId hLId).elim
              · exact ⟨l, hTailMem, hLId⟩
            have hTail := ih hTailPair hTailTarget hTailOld hTailMem
            cases hHeadValue : p head <;>
              simp [List.map, List.filter, hId, hHeadMap, hHeadValue, hTail]

theorem map_updateLineageKind_filter_length_eq_of_target
    {ls : List LeaseLineage} {id : Nat} {kind : LineageKind}
    {p : LeaseLineage → Bool}
    (hPair : ls.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hTarget : ∀ l ∈ ls, l.id = id → p { l with kind := kind } = p l)
    (hMemTarget : ∃ l ∈ ls, l.id = id) :
    ((ls.map (fun l => if l.id = id then { l with kind := kind } else l)).filter p).length =
      (ls.filter p).length := by
  induction ls with
  | nil =>
      rcases hMemTarget with ⟨l, hMem, hId⟩
      contradiction
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTailPair =>
          by_cases hId : head.id = id
          · have hHeadMap : (if head.id = id then { head with kind := kind } else head) =
                { head with kind := kind } := by simp [hId]
            have hHeadP : p { id := id, kind := kind, cloneCount := head.cloneCount } = p head := by
              simpa [hId] using hTarget head List.mem_cons_self hId
            have hNoTail : ∀ l ∈ tail, l.id ≠ id := by
              intro l hMem hLId
              exact hHead l hMem (hId.trans hLId.symm)
            have hTailMap := map_updateLineageKind_eq_self_of_no_id
              (ls := tail) (id := id) (kind := kind) hNoTail
            cases hHeadValue : p head with
            | false =>
                simp [List.map, List.filter, hId, hHeadMap, hHeadP, hHeadValue, hTailMap]
            | true =>
                simp [List.map, List.filter, hId, hHeadMap, hHeadP, hHeadValue, hTailMap]
          · have hHeadMap : (if head.id = id then { head with kind := kind } else head) = head := by
                simp [hId]
            have hTailMem : ∃ l ∈ tail, l.id = id := by
              rcases hMemTarget with ⟨l, hMem, hLId⟩
              rcases List.mem_cons.mp hMem with hEq | hTailMem
              · subst l
                exact (hId hLId).elim
              · exact ⟨l, hTailMem, hLId⟩
            have hTailTarget : ∀ l ∈ tail, l.id = id →
                p { l with kind := kind } = p l := by
              intro l hMem hLId
              exact hTarget l (List.mem_cons_of_mem head hMem) hLId
            have hTail := ih hTailPair hTailTarget hTailMem
            cases hHeadValue : p head <;>
              simpa [List.map, List.filter, hId, hHeadMap, hHeadValue] using hTail

theorem unit_append_tentative
    {ls : List LeaseLineage} {readerId id : Nat}
    (hUnit : ∀ l ∈ ls, (∃ oldReaderId, l.kind = .tentativeFast oldReaderId) → l.cloneCount = 1) :
    ∀ l ∈ ls ++ [{ id := id, kind := .tentativeFast readerId, cloneCount := 1 }],
      (∃ oldReaderId, l.kind = .tentativeFast oldReaderId) → l.cloneCount = 1 := by
  intro l hMem hTentative
  simp only [List.mem_append, List.mem_singleton] at hMem
  cases hMem with
  | inl hOld => exact hUnit l hOld hTentative
  | inr hNew =>
      subst hNew
      rfl

theorem tentative_mem_kind
    {c : ConcreteState} {l : LeaseLineage}
    (hMem : l ∈ c.tentativeLineages) :
    ∃ readerId, l.kind = .tentativeFast readerId := by
  cases hKind : l.kind with
  | tentativeFast readerId => exact ⟨readerId, by simpa using hKind⟩
  | validatedFast readerId => simp [ConcreteState.tentativeLineages, hKind] at hMem
  | slow token => simp [ConcreteState.tentativeLineages, hKind] at hMem

theorem validated_mem_kind
    {c : ConcreteState} {l : LeaseLineage}
    (hMem : l ∈ c.validatedFastLineages) :
    ∃ readerId, l.kind = .validatedFast readerId := by
  cases hKind : l.kind with
  | tentativeFast readerId => simp [ConcreteState.validatedFastLineages, hKind] at hMem
  | validatedFast readerId => exact ⟨readerId, by simpa using hKind⟩
  | slow token => simp [ConcreteState.validatedFastLineages, hKind] at hMem

theorem slow_mem_kind
    {c : ConcreteState} {l : LeaseLineage}
    (hMem : l ∈ c.slowLineages) :
    ∃ token, l.kind = .slow token := by
  cases hKind : l.kind with
  | tentativeFast readerId => simp [ConcreteState.slowLineages, hKind] at hMem
  | validatedFast readerId => simp [ConcreteState.slowLineages, hKind] at hMem
  | slow token => exact ⟨token, by simpa using hKind⟩

theorem length_tentative_updateFastLookupStage_tentative
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hObserved : lookup.stage = .observed) :
    ((l.map (fun x => if x.id = id then { x with stage := .tentative } else x)).filter
        (fun x => decide (x.stage = .tentative))).length =
      (l.filter (fun x => decide (x.stage = .tentative))).length + 1 := by
  induction l with
  | nil => simp at hFind
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTail =>
          by_cases hId : head.id = id
          · have hHeadId : head.id = id := hId
            have hEq : head = lookup := by
              simpa [List.find?, hId] using hFind
            subst lookup
            have hNoTail : ∀ x ∈ tail, x.id ≠ id := by
              intro x hx hX
              apply hHead x hx
              exact hHeadId.trans hX.symm
            have hTailMap := map_updateFastLookupStage_eq_self_of_id_ne
              (l := tail) (id := id) (stage := FastLookupStage.tentative) hNoTail
            have hOldHead : decide (head.stage = FastLookupStage.tentative) = false := by
              simp [hObserved]
            simp only [List.map, if_pos hId]
            rw [hTailMap]
            simp [List.filter, hOldHead]
          · have hIdFalse : (head.id == id) = false := by
              exact Bool.not_eq_true _ |>.mp (by simpa using hId)
            have hFindTail : tail.find? (fun x => x.id == id) = some lookup := by
              simpa [List.find?, hIdFalse] using hFind
            have hTailResult := ih hTail hFindTail
            rw [List.map, if_neg hId]
            by_cases hStage : decide (head.stage = FastLookupStage.tentative) = true
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult

theorem length_tentative_updateFastLookupStage_validated
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hTentative : lookup.stage = .tentative) :
    ((l.map (fun x => if x.id = id then { x with stage := .validated } else x)).filter
        (fun x => decide (x.stage = .tentative))).length + 1 =
      (l.filter (fun x => decide (x.stage = .tentative))).length := by
  induction l with
  | nil => simp at hFind
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTail =>
          by_cases hId : head.id = id
          · have hHeadId : head.id = id := hId
            have hEq : head = lookup := by
              simpa [List.find?, hId] using hFind
            subst lookup
            have hNoTail : ∀ x ∈ tail, x.id ≠ id := by
              intro x hx hX
              apply hHead x hx
              exact hHeadId.trans hX.symm
            have hTailMap := map_updateFastLookupStage_eq_self_of_id_ne
              (l := tail) (id := id) (stage := FastLookupStage.validated) hNoTail
            have hOldHead : decide (head.stage = FastLookupStage.tentative) = true := by
              simp [hTentative]
            simp only [List.map, if_pos hId]
            rw [hTailMap]
            simp [List.filter, hOldHead]
          · have hIdFalse : (head.id == id) = false := by
              exact Bool.not_eq_true _ |>.mp (by simpa using hId)
            have hFindTail : tail.find? (fun x => x.id == id) = some lookup := by
              simpa [List.find?, hIdFalse] using hFind
            have hTailResult := ih hTail hFindTail
            rw [List.map, if_neg hId]
            by_cases hStage : decide (head.stage = FastLookupStage.tentative) = true
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult

theorem tentative_removeFastLookup_filter_eq
    {l : List FastLookup} {id : Nat} :
    (l.filter (fun x => x.id != id)).filter
        (fun x => decide (x.stage = .tentative)) =
      (l.filter (fun x => decide (x.stage = .tentative))).filter
        (fun x => x.id != id) := by
  rw [List.filter_filter, List.filter_filter]
  simp [Bool.and_comm]

theorem tentative_filter_and_id_ne_eq
    {l : List FastLookup} {id : Nat}
    (hNe : ∀ lookup ∈ l, lookup.id ≠ id) :
    l.filter (fun x => decide (x.stage = .tentative) && x.id != id) =
      l.filter (fun x => decide (x.stage = .tentative)) := by
  induction l with
  | nil => rfl
  | cons head tail ih =>
      have hHeadNe := hNe head List.mem_cons_self
      have hTailNe : ∀ lookup ∈ tail, lookup.id ≠ id := by
        intro lookup hMem
        exact hNe lookup (List.mem_cons_of_mem head hMem)
      have hIdNe : (head.id != id) = true := bne_iff_ne.mpr hHeadNe
      by_cases hStage : decide (head.stage = FastLookupStage.tentative) = true
      · simp [List.filter, hIdNe, hStage, ih hTailNe]
      · simp [List.filter, hIdNe, hStage, ih hTailNe]

theorem tentative_removeFastLookup_eq
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hNotTentative : lookup.stage ≠ .tentative) :
    (l.filter (fun x => x.id != id)).filter
        (fun x => decide (x.stage = .tentative)) =
      l.filter (fun x => decide (x.stage = .tentative)) := by
  rw [List.filter_filter]
  induction l with
  | nil => simp at hFind
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTail =>
          by_cases hId : head.id = id
          · have hHeadId : head.id = id := hId
            have hEq : head = lookup := by
              simpa [List.find?, hId] using hFind
            subst lookup
            have hIdNe : (head.id != id) = false := by simp [hHeadId]
            have hStageFalse : decide (head.stage = FastLookupStage.tentative) = false := by
              simp [hNotTentative]
            have hTailNe : ∀ x ∈ tail, x.id ≠ id := by
              intro x hx hX
              apply hHead x hx
              exact hHeadId.trans hX.symm
            have hTailFilter := tentative_filter_and_id_ne_eq
              (l := tail) (id := id) hTailNe
            simp [List.filter, hIdNe, hStageFalse, hTailFilter]
          · have hIdFalse : (head.id == id) = false := by
              exact Bool.not_eq_true _ |>.mp (by simpa using hId)
            have hFindTail : tail.find? (fun x => x.id == id) = some lookup := by
              simpa [List.find?, hIdFalse] using hFind
            have hTailResult := ih hTail hFindTail
            have hIdNe : (head.id != id) = true := bne_iff_ne.mpr hId
            by_cases hStage : decide (head.stage = FastLookupStage.tentative) = true
            · simpa [List.filter, hIdFalse, hIdNe, hStage] using hTailResult
            · simpa [List.filter, hIdFalse, hIdNe, hStage] using hTailResult

theorem length_tentative_removeFastLookup
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hTentative : lookup.stage = .tentative) :
    ((l.filter (fun x => x.id != id)).filter
        (fun x => decide (x.stage = .tentative))).length + 1 =
      (l.filter (fun x => decide (x.stage = .tentative))).length := by
  rw [tentative_removeFastLookup_filter_eq]
  have hMem := List.mem_of_find?_eq_some hFind
  have hId := List.find?_some hFind
  have hTentMem : lookup ∈ l.filter (fun x => decide (x.stage = .tentative)) := by
    apply List.mem_filter.mpr
    exact ⟨hMem, by simp [hTentative]⟩
  have hPairTent := pairwise_filter
    (fun x => decide (x.stage = FastLookupStage.tentative)) hPair
  have hLen := length_filter_ne_of_mem hPairTent hTentMem
  have hLookupId : lookup.id = id := beq_iff_eq.mp hId
  rw [hLookupId] at hLen
  exact hLen

theorem pairwise_mem_ne_local {α : Type} {R : α → α → Prop} {x y : α} {l : List α}
    (hP : l.Pairwise R) (hX : x ∈ l) (hY : y ∈ l) (hNe : x ≠ y) :
    R x y ∨ R y x := by
  induction hP with
  | nil => contradiction
  | cons hHead hTail ih =>
      cases List.mem_cons.mp hX with
      | inl hX1 =>
          subst hX1
          cases List.mem_cons.mp hY with
          | inl hY1 => subst hY1; contradiction
          | inr hY2 => left; exact hHead y hY2
      | inr hX2 =>
          cases List.mem_cons.mp hY with
          | inl hY1 => subst hY1; right; exact hHead x hX2
          | inr hY2 => exact ih hX2 hY2

theorem fastLookup_eq_of_mem_id_eq
    {l : List FastLookup} {lhs rhs : FastLookup}
    (hPair : l.Pairwise (fun a b => a.id ≠ b.id))
    (hLhs : lhs ∈ l) (hRhs : rhs ∈ l)
    (hId : lhs.id = rhs.id) :
    lhs = rhs := by
  by_cases hEq : lhs = rhs
  · exact hEq
  · have hRel := pairwise_mem_ne_local hPair hLhs hRhs hEq
    rcases hRel with hRel | hRel
    · exact (hRel hId).elim
    · exact (hRel hId.symm).elim

theorem length_lineage_filter_ne_of_mem
    {l : List LeaseLineage} {lineage : LeaseLineage} {id : Nat}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hMem : lineage ∈ l)
    (hLineageId : lineage.id = id) :
    (l.filter (fun x => x.id != id)).length + 1 = l.length := by
  induction l with
  | nil => contradiction
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTail =>
          dsimp [List.filter]
          cases List.mem_cons.mp hMem with
          | inl hEq =>
              subst lineage
              have hFilterId : tail.filter (fun x => x.id != id) = tail := by
                apply List.filter_eq_self.mpr
                intro x hx
                have hNe : x.id ≠ id := by
                  intro hEqId
                  exact (hHead x hx) (hLineageId.trans hEqId.symm)
                exact bne_iff_ne.mpr hNe
              have hHeadBne : (head.id != id) = false := by simp [hLineageId]
              rw [hHeadBne, hFilterId]
          | inr hInTail =>
              have hNe : head.id ≠ id := by
                intro hEqId
                exact (hHead lineage hInTail) (hEqId.trans hLineageId.symm)
              have hBne : (head.id != id) = true := bne_iff_ne.mpr hNe
              rw [hBne]
              simp only [List.length_cons]
              have hTailLength := ih hTail hInTail
              omega


inductive ConcreteEvent where
  | baseStep (e : Event)
  | cloneLineage (id : Nat)
  | dropCloneNonFinal (id : Nat)
  | dropCloneFinalFast (readerId : Nat) (lineageId : Nat)
  | dropCloneFinalSlow (lineageId : Nat)
deriving DecidableEq, Repr

def Event.changesLineage : Event → Bool
  | .acquireTentativeLease _
  | .rejectTentativeFastLookup _
  | .validateFastLookup _
  | .fallbackFastLookup _
  | .beginSlowLookup _
  | .completeFastLookup _
  | .endSlowLookup => true
  | _ => false

inductive ConcreteStep : ConcreteState → ConcreteEvent → ConcreteState → Prop where
  | baseStep
      {c : ConcreteState} {e : Event} {s' : State}
      (hStep : Step c.snapshot e s')
      (hNoLineageChange : e.changesLineage = false) :
      ConcreteStep c (.baseStep e) { c with snapshot := s' }

  | acquireTentativeLeaseLineage
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      (hNoLineage : ∀ l ∈ c.lineages, l.id ≠ lineageId)
      (hLineageId : lineageId = readerId)
      (hStep : Step c.snapshot (.acquireTentativeLease readerId) s') :
      ConcreteStep c (.baseStep (.acquireTentativeLease readerId))
        { snapshot := s'
          lineages := c.lineages ++
            [{ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 }] }

  | rejectTentativeFastLookupLineage
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = lineageId)
      (hKind : lineage.kind = .tentativeFast readerId)
      (hLineageId : lineageId = readerId)
      (hOne : lineage.cloneCount = 1)
      (hStep : Step c.snapshot (.rejectTentativeFastLookup readerId) s') :
      ConcreteStep c (.baseStep (.rejectTentativeFastLookup readerId))
        { snapshot := s'
          lineages := c.removeLineage lineageId }

  | validateFastLookupLineage
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = lineageId)
      (hKind : lineage.kind = .tentativeFast readerId)
      (hLineageId : lineageId = readerId)
      (hStep : Step c.snapshot (.validateFastLookup readerId) s') :
      ConcreteStep c (.baseStep (.validateFastLookup readerId))
        { snapshot := s'
          lineages := c.updateLineageKind lineageId (.validatedFast readerId) }

  | fallbackFastLookupLineage
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = lineageId)
      (hKind : lineage.kind = .validatedFast readerId)
      (hLineageId : lineageId = readerId)
      (hOne : lineage.cloneCount = 1)
      (hStep : Step c.snapshot (.fallbackFastLookup readerId) s') :
      ConcreteStep c (.baseStep (.fallbackFastLookup readerId))
        { snapshot := s'
          lineages := c.removeLineage lineageId }

  | beginSlowLookupLineage
      {c : ConcreteState} {s' : State} {token : Token} {lineageId : Nat}
      (hNoLineage : ∀ l ∈ c.lineages, l.id ≠ lineageId)
      (hStep : Step c.snapshot (.beginSlowLookup token) s') :
      ConcreteStep c (.baseStep (.beginSlowLookup token))
        { snapshot := s'
          lineages := c.lineages ++ [{ id := lineageId, kind := .slow token, cloneCount := 1 }] }

  | cloneLineage
      {c : ConcreteState} {id : Nat} {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = id)
      (hNotTentative : ∀ readerId, lineage.kind ≠ .tentativeFast readerId) :
      ConcreteStep c (.cloneLineage id)
        { c with lineages := c.updateLineageCloneCount id (lineage.cloneCount + 1) }

  | dropCloneNonFinal
      {c : ConcreteState} {id : Nat} {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = id)
      (hGtOne : lineage.cloneCount > 1)
      (hNotTentative : ∀ readerId, lineage.kind ≠ .tentativeFast readerId) :
      ConcreteStep c (.dropCloneNonFinal id)
        { c with lineages := c.updateLineageCloneCount id (lineage.cloneCount - 1) }

  | dropCloneFinalFast
      {c : ConcreteState} {s' : State} {readerId : Nat} {lineageId : Nat}
      {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = lineageId)
      (hKind : lineage.kind = .validatedFast readerId)
      (hLineageId : lineageId = readerId)
      (hOne : lineage.cloneCount = 1)
      (hStep : Step c.snapshot (.completeFastLookup readerId) s') :
      ConcreteStep c (.dropCloneFinalFast readerId lineageId)
        { snapshot := s'
          lineages := c.removeLineage lineageId }

  | dropCloneFinalSlow
      {c : ConcreteState} {s' : State} {token : Token} {lineageId : Nat}
      {lineage : LeaseLineage}
      (hMem : lineage ∈ c.lineages)
      (hId : lineage.id = lineageId)
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

theorem lineages_sound_transport
    {c : ConcreteState} {s : State}
    (hSound : c.LineagesSound)
    (hFast : ∀ lookup ∈ c.snapshot.fastLookups, lookup.stage ≠ .observed →
      ∃ lookup' ∈ s.fastLookups,
        lookup'.id = lookup.id ∧ lookup'.token = lookup.token ∧ lookup'.stage = lookup.stage)
    (hPub : ∀ pub ∈ c.snapshot.publications,
      ∃ pub' ∈ s.publications, pub'.slot = pub.slot ∧ pub'.generation = pub.generation)
    (hSession : s.registry.session = c.snapshot.registry.session) :
    ∀ l ∈ c.lineages,
      match l.kind with
      | .tentativeFast readerId =>
          ∃ lookup ∈ s.fastLookups,
            lookup.id = readerId ∧ lookup.stage = .tentative ∧ l.id = readerId
      | .validatedFast readerId =>
          ∃ lookup ∈ s.fastLookups,
            lookup.id = readerId ∧ lookup.stage = .validated ∧ l.id = readerId
      | .slow token => token.session = s.registry.session := by
  intro l hMem
  cases hKind : l.kind with
  | tentativeFast readerId =>
      have hLine := hSound l hMem
      simp [hKind] at hLine
      rcases hLine with ⟨lookup, hLookup, hId, hStage, hLineageId⟩
      rcases hFast lookup hLookup (by simp [hStage]) with
        ⟨lookup', hLookup', hId', hToken', hStage'⟩
      exact ⟨lookup', hLookup', by simpa [hId] using hId'.trans hId,
        by simpa [hStage] using hStage', hLineageId⟩
  | validatedFast readerId =>
      have hLine := hSound l hMem
      simp [hKind] at hLine
      rcases hLine with ⟨lookup, hLookup, hId, hStage, hLineageId⟩
      rcases hFast lookup hLookup (by simp [hStage]) with
        ⟨lookup', hLookup', hId', hToken', hStage'⟩
      exact ⟨lookup', hLookup', by simpa [hId] using hId'.trans hId,
        by simpa [hStage] using hStage', hLineageId⟩
  | slow token =>
      have hToken := hSound l hMem
      simpa [hKind, hSession] using hToken

theorem invariant_with_same_lineages
    {c : ConcreteState} {s : State}
    (hInv : c.Invariant)
    (hSnap : s.Invariant)
    (hFast : ∀ lookup ∈ c.snapshot.fastLookups, lookup.stage ≠ .observed →
      ∃ lookup' ∈ s.fastLookups,
        lookup'.id = lookup.id ∧ lookup'.token = lookup.token ∧ lookup'.stage = lookup.stage)
    (hPub : ∀ pub ∈ c.snapshot.publications,
      ∃ pub' ∈ s.publications, pub'.slot = pub.slot ∧ pub'.generation = pub.generation)
    (hSession : s.registry.session = c.snapshot.registry.session)
    (hAccounting :
      (c.tentativeLineages.length = s.tentativeFastLookups.length) ∧
      (c.validatedFastLineages.length = s.validatedFastLookups.length) ∧
      (c.totalTentativePhysicalLeases = c.tentativeLineages.length) ∧
      (c.totalCommittedLineages = s.registry.activeLeases)) :
    ({ snapshot := s, lineages := c.lineages } : ConcreteState).Invariant := by
  rcases hInv with ⟨_, hUnique, hPositive, hUnit, hSound, _⟩
  have hSound' := lineages_sound_transport (c := c) (s := s) hSound hFast hPub hSession
  exact ⟨hSnap, hUnique, hPositive, hUnit, hSound', hAccounting⟩

theorem base_step_invariant_preserved
    {c : ConcreteState} {e : Event} {s' : State}
    (hInv : c.Invariant)
    (hStep : Step c.snapshot e s')
    (hNoLineageChange : e.changesLineage = false) :
    ({ snapshot := s', lineages := c.lineages } : ConcreteState).Invariant := by
  have hInvParts := hInv
  rcases hInvParts with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
  have hSnap' := Step.invariant_preserved hSnap hStep
  cases hStep with
  | insertFresh hReg hNoSnap hNoPub =>
      cases hReg with
      | insertFresh hMay =>
          apply invariant_with_same_lineages hInv hSnap'
          · intro lookup hMem hStage
            exact ⟨lookup, hMem, rfl, rfl, rfl⟩
          · intro pub hMem
            exact ⟨pub, List.mem_append_left _ hMem, rfl, rfl⟩
          · rfl
          · simpa [ConcreteState.LineageAccounting, State.tentativeFastLookups,
              State.validatedFastLookups] using hAccounting
  | insertReuse hReg hNoSnap hNoPub =>
      cases hReg with
      | insertReuse hMay hInBounds hVacant =>
          apply invariant_with_same_lineages hInv hSnap'
          · intro lookup hMem hStage
            exact ⟨lookup, hMem, rfl, rfl, rfl⟩
          · intro pub hMem
            exact ⟨pub, List.mem_append_left _ hMem, rfl, rfl⟩
          · rfl
          · simpa [ConcreteState.LineageAccounting, State.tentativeFastLookups,
              State.validatedFastLookups] using hAccounting
  | removeReuse hReg hPub hLive =>
      cases hReg with
      | removeReuse hAuth hInBounds hLiveSlot hNextGen =>
          rename_i targetToken targetGeneration targetPub
          apply invariant_with_same_lineages hInv hSnap'
          · intro lookup hMem hStage
            exact ⟨lookup, hMem, rfl, rfl, rfl⟩
          · intro pub hMem
            let mapped : Publication :=
              if pub.slot == targetToken.slot && pub.generation == targetToken.generation then
                { pub with state := .stale }
              else pub
            refine ⟨mapped, List.mem_map.mpr ⟨pub, hMem, ?_⟩, ?_, ?_⟩
            · dsimp [State.updatePublicationState, mapped]
            · dsimp [mapped]
              split <;> rfl
            · dsimp [mapped]
              split <;> rfl
          · rfl
          · simpa [ConcreteState.LineageAccounting, State.tentativeFastLookups,
              State.validatedFastLookups] using hAccounting
  | removeRetire hReg hPub hLive =>
      cases hReg with
      | removeRetire hAuth hInBounds hLiveSlot hExhausted =>
          rename_i targetToken targetPub
          apply invariant_with_same_lineages hInv hSnap'
          · intro lookup hMem hStage
            exact ⟨lookup, hMem, rfl, rfl, rfl⟩
          · intro pub hMem
            let mapped : Publication :=
              if pub.slot == targetToken.slot && pub.generation == targetToken.generation then
                { pub with state := .stale }
              else pub
            refine ⟨mapped, List.mem_map.mpr ⟨pub, hMem, ?_⟩, ?_, ?_⟩
            · dsimp [State.updatePublicationState, mapped]
            · dsimp [mapped]
              split <;> rfl
            · dsimp [mapped]
              split <;> rfl
          · rfl
          · simpa [ConcreteState.LineageAccounting, State.tentativeFastLookups,
              State.validatedFastLookups] using hAccounting
  | beginFastObservation hNoReader hSnapBinding hSnapGen hPub hAuth hLive =>
      apply invariant_with_same_lineages hInv hSnap'
      · intro lookup hMem hStage
        exact ⟨lookup, List.mem_append_left _ hMem, rfl, rfl, rfl⟩
      · intro pub hMem
        exact ⟨pub, hMem, rfl, rfl⟩
      · rfl
      · simpa [ConcreteState.LineageAccounting, State.tentativeFastLookups,
          State.validatedFastLookups] using hAccounting
  | abandonObservation hLookup hObserved hNotOpen =>
      rename_i readerId targetLookup
      have ⟨hTargetMem, hTargetId⟩ := findFastLookup?_mem_and_id hLookup
      apply invariant_with_same_lineages hInv hSnap'
      · intro lookup hMem hStage
        have hNe : lookup.id ≠ targetLookup.id := by
          intro hEq
          have hSame := fastLookup_eq_of_mem_id_eq hSnap.2.2.1 hMem hTargetMem hEq
          subst hSame
          exact hStage hObserved
        have hNe' : lookup.id ≠ readerId := by
          intro hEq
          apply hNe
          exact hEq.trans hTargetId.symm
        exact ⟨lookup, List.mem_filter.mpr ⟨hMem, by simp [hNe']⟩, rfl, rfl, rfl⟩
      · intro pub hMem
        exact ⟨pub, hMem, rfl, rfl⟩
      · rfl
      · dsimp [ConcreteState.LineageAccounting, State.tentativeFastLookups,
          State.validatedFastLookups, State.removeFastLookup]
        have hTent := tentative_removeFastLookup_eq hSnap.2.2.1 hLookup (by simp [hObserved])
        have hVal := validated_removeFastLookup_eq hSnap.2.2.1 hLookup (by simp [hObserved])
        rw [hTent, hVal]
        exact hAccounting
  | beginSealLeaseAdmission hOpen =>
      apply invariant_with_same_lineages hInv hSnap'
      · intro lookup hMem hStage
        exact ⟨lookup, hMem, rfl, rfl, rfl⟩
      · intro pub hMem
        exact ⟨pub, hMem, rfl, rfl⟩
      · rfl
      · exact hAccounting
  | finishSealLeaseAdmission hSealing =>
      apply invariant_with_same_lineages hInv hSnap'
      · intro lookup hMem hStage
        exact ⟨lookup, hMem, rfl, rfl, rfl⟩
      · intro pub hMem
        exact ⟨pub, hMem, rfl, rfl⟩
      · rfl
      · exact hAccounting
  | closeRegistry hSealed hReg =>
      cases hReg with
      | closeRegistry hNotClosed =>
          apply invariant_with_same_lineages hInv hSnap'
          · intro lookup hMem hStage
            exact ⟨lookup, hMem, rfl, rfl, rfl⟩
          · intro pub hMem
            let mapped : Publication :=
              if pub.state = .live then { pub with state := .closing } else pub
            refine ⟨mapped, List.mem_map.mpr ⟨pub, hMem, ?_⟩, ?_, ?_⟩
            · dsimp [State.updateClosingPublications, mapped]
            · dsimp [mapped]
              split <;> rfl
            · dsimp [mapped]
              split <;> rfl
          · rfl
          · exact hAccounting
  | finishClose hNoTentative hNoValidated hReg =>
      cases hReg with
      | finishClose hClosed hNoLeases =>
          exact ⟨hSnap', hUnique, hPositive, hUnit, hSound, hAccounting⟩
  | acquireTentativeLease hLookup hObserved hNotSealed hNotClosed =>
      simp [Event.changesLineage] at hNoLineageChange
  | validateFastLookup hLookup hTentative hPub hLive hReg =>
      simp [Event.changesLineage] at hNoLineageChange
  | rejectTentativeFastLookup hLookup hTentative hPub hNotLive =>
      simp [Event.changesLineage] at hNoLineageChange
  | completeFastLookup hLookup hValidated hReg =>
      simp [Event.changesLineage] at hNoLineageChange
  | fallbackFastLookup hLookup hValidated hPub hNotLive hReg =>
      simp [Event.changesLineage] at hNoLineageChange
  | beginSlowLookup hNotSealed hReg =>
      simp [Event.changesLineage] at hNoLineageChange
  | endSlowLookup hSlowLease hReg =>
      simp [Event.changesLineage] at hNoLineageChange

theorem map_updateLineageCloneCount_filter_length_eq
    {ls : List LeaseLineage} {id newCount : Nat} {p : LeaseLineage → Bool}
    (hP : ∀ l, p { l with cloneCount := newCount } = p l) :
    ((ls.map (fun l => if l.id = id then { l with cloneCount := newCount } else l)).filter p).length =
      (ls.filter p).length := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      by_cases hId : head.id = id
      · have hHead := hP head
        have hHeadMap : (if head.id = id then
            { head with cloneCount := newCount } else head) =
            { head with cloneCount := newCount } := by simp [hId]
        simp only [List.map]
        simp only [List.filter]
        cases hPHead : p head <;> simp [hHeadMap, hHead, hPHead, ih]
      · have hHeadMap : (if head.id = id then
            { head with cloneCount := newCount } else head) = head := by simp [hId]
        simp only [List.map]
        simp only [List.filter]
        cases hPHead : p head <;> simp [hHeadMap, hPHead, ih]

theorem map_updateLineageCloneCount_filter_eq_of_target_false
    {ls : List LeaseLineage} {id newCount : Nat} {p : LeaseLineage → Bool}
    (hP : ∀ l, p { l with cloneCount := newCount } = p l)
    (hTarget : ∀ l ∈ ls, l.id = id → p { l with cloneCount := newCount } = false) :
    (ls.map (fun l => if l.id = id then { l with cloneCount := newCount } else l)).filter p =
      ls.filter p := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      by_cases hId : head.id = id
      · have hHead := hP head
        have hTargetHead := hTarget head List.mem_cons_self hId
        have hHeadMap : (if head.id = id then
            { head with cloneCount := newCount } else head) =
            { head with cloneCount := newCount } := by simp [hId]
        have hHeadFalse : p head = false := by
          rw [← hHead]
          exact hTargetHead
        have hTail := ih (by
          intro l hMem hLId
          exact hTarget l (List.mem_cons_of_mem head hMem) hLId)
        simp [List.map, List.filter, hHeadMap, hTargetHead, hHeadFalse, hTail]
      · have hHeadMap : (if head.id = id then
            { head with cloneCount := newCount } else head) = head := by simp [hId]
        have hTail := ih (by
          intro l hMem hLId
          exact hTarget l (List.mem_cons_of_mem head hMem) hLId)
        simp [List.map, List.filter, hHeadMap, hTail]

theorem removeLineage_filter_eq
    {c : ConcreteState} {id : Nat} {p : LeaseLineage → Bool} :
    (c.removeLineage id).filter p =
      (c.lineages.filter p).filter (fun l => l.id != id) := by
  simp [ConcreteState.removeLineage, List.filter_filter, Bool.and_comm]

theorem filter_id_ne_eq_of_no_id
    {ls : List LeaseLineage} {id : Nat} {p : LeaseLineage → Bool}
    (hNoId : ∀ l ∈ ls, p l = true → l.id ≠ id) :
    (ls.filter (fun l => l.id != id)).filter p = ls.filter p := by
  induction ls with
  | nil => rfl
  | cons head tail ih =>
      have hHeadNoId := hNoId head List.mem_cons_self
      have hTailNoId : ∀ l ∈ tail, p l = true → l.id ≠ id := by
        intro l hMem hP
        exact hNoId l (List.mem_cons_of_mem head hMem) hP
      by_cases hId : head.id = id
      · have hIdFalse : (head.id != id) = false := by simp [hId]
        by_cases hP : p head = true
        · exact (hHeadNoId hP hId).elim
        · simp [List.filter, hIdFalse, hP, ih hTailNoId]
      · have hIdTrue : (head.id != id) = true := by simp [hId]
        by_cases hP : p head = true
        · simp [List.filter, hIdTrue, hP, ih hTailNoId]
        · simp [List.filter, hIdTrue, hP, ih hTailNoId]

theorem updateLineageCloneCount_tentative_eq
    {c : ConcreteState} {id newCount : Nat} :
    ((c.updateLineageCloneCount id newCount).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false)).length = c.tentativeLineages.length := by
  exact map_updateLineageCloneCount_filter_length_eq
    (p := fun l => match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false)
    (fun l => by cases l <;> rfl)

theorem updateLineageCloneCount_validated_eq
    {c : ConcreteState} {id newCount : Nat} :
    ((c.updateLineageCloneCount id newCount).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false)).length = c.validatedFastLineages.length := by
  exact map_updateLineageCloneCount_filter_length_eq
    (p := fun l => match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false)
    (fun l => by cases l <;> rfl)

theorem updateLineageCloneCount_slow_eq
    {c : ConcreteState} {id newCount : Nat} :
    ((c.updateLineageCloneCount id newCount).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => false
      | .slow _ => true)).length = c.slowLineages.length := by
  exact map_updateLineageCloneCount_filter_length_eq
    (p := fun l => match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => false
      | .slow _ => true)
    (fun l => by cases l <;> rfl)

theorem terminal_quiescence_equivalence
    {c : ConcreteState}
    (hInv : c.Invariant)
    (hPhysicalZero : c.totalPhysicalLeases = 0) :
    c.snapshot.registry.activeLeases = 0 ∧
    c.snapshot.validatedFastLookups = [] ∧
    c.snapshot.tentativeFastLookups = [] ∧
    c.lineages = [] := by
  have hLineagesNil := (zero_lineages_iff_zero_physical_leases hInv.2.2.1).mpr hPhysicalZero
  have hAcc := hInv.2.2.2.2.2
  dsimp [ConcreteState.LineageAccounting] at hAcc
  have hTentLineages : c.tentativeLineages = [] := by
    simp [ConcreteState.tentativeLineages, hLineagesNil]
  have hValidatedLineages : c.validatedFastLineages = [] := by
    simp [ConcreteState.validatedFastLineages, hLineagesNil]
  have hSlowLineages : c.slowLineages = [] := by
    simp [ConcreteState.slowLineages, hLineagesNil]
  dsimp [ConcreteState.totalTentativePhysicalLeases,
    ConcreteState.totalCommittedLineages] at hAcc
  rw [hTentLineages, hValidatedLineages, hSlowLineages] at hAcc
  have hAct : c.snapshot.registry.activeLeases = 0 := hAcc.2.2.2.symm
  have hValLen : c.snapshot.validatedFastLookups.length = 0 := by
    have hLe := hAcc.2.1
    omega
  have hValNil : c.snapshot.validatedFastLookups = [] :=
    List.length_eq_zero_iff.mp hValLen
  have hTentLen : c.snapshot.tentativeFastLookups.length = 0 := by
    have hLe := hAcc.1
    omega
  have hTentNil : c.snapshot.tentativeFastLookups = [] :=
    List.length_eq_zero_iff.mp hTentLen
  exact ⟨hAct, hValNil, hTentNil, hLineagesNil⟩

theorem acquire_tentative_lease_invariant_preserved
    {c c' : ConcreteState} {readerId lineageId : Nat} {s' : State}
    (hInv : c.Invariant)
    (hNoLineage : ∀ l ∈ c.lineages, l.id ≠ lineageId)
    (hLineageId : lineageId = readerId)
    (hStep : Step c.snapshot (.acquireTentativeLease readerId) s')
    (hEq : c' = { snapshot := s', lineages := c.lineages ++
      [{ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 }] }) :
    c'.Invariant := by
  subst c'
  rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
  dsimp [ConcreteState.TentativeLineagesUnit] at hUnit
  have hFastUnique := hSnap.2.2.1
  have hSnap' := Step.invariant_preserved hSnap hStep
  cases hStep with
  | acquireTentativeLease hLookup hObserved hNotSealed hNotClosed =>
      rename_i lookup
      have ⟨hTargetMem, hTargetId⟩ := findFastLookup?_mem_and_id hLookup
      have hUnique' : (c.lineages ++
          [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).Pairwise
          (fun (lhs : LeaseLineage) (rhs : LeaseLineage) => lhs.id ≠ rhs.id) := by
        apply pairwise_append_singleton hUnique
        intro y hy
        exact hNoLineage y hy
      have hPositive' : ∀ l ∈ c.lineages ++
          [{ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 }],
          l.cloneCount > 0 := by
        intro l hMem
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld => exact hPositive l hOld
        | inr hNew => simp_all
      have hUnit' := unit_append_tentative (id := lineageId) (readerId := readerId) hUnit
      refine ⟨hSnap', hUnique', hPositive', hUnit', ?_, ?_⟩
      · intro l hMem
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld =>
            cases hKind : l.kind with
            | tentativeFast oldReaderId =>
                have hLine := hSound l hOld
                simp [hKind] at hLine
                rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                have hOldNe : oldLookup.id ≠ readerId := by
                  intro hEq
                  have hSame := fastLookup_eq_of_mem_id_eq
                    hFastUnique oldMem hTargetMem (hEq.trans hTargetId.symm)
                  subst hSame
                  simp_all
                refine ⟨oldLookup, List.mem_map.mpr ⟨oldLookup, oldMem, ?_⟩,
                  oldId, ?_, oldLineageId⟩
                · simp [hOldNe]
                · simpa [hKind] using oldStage
            | validatedFast oldReaderId =>
                have hLine := hSound l hOld
                simp [hKind] at hLine
                rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                have hOldNe : oldLookup.id ≠ readerId := by
                  intro hEq
                  have hSame := fastLookup_eq_of_mem_id_eq
                    hFastUnique oldMem hTargetMem (hEq.trans hTargetId.symm)
                  subst hSame
                  simp_all
                refine ⟨oldLookup, List.mem_map.mpr ⟨oldLookup, oldMem, ?_⟩,
                  oldId, ?_, oldLineageId⟩
                · simp [hOldNe]
                · simpa [hKind] using oldStage
            | slow token =>
                have hToken := hSound l hOld
                simpa [hKind] using hToken
        | inr hNew =>
            subst hNew
            refine ⟨{ lookup with stage := .tentative }, ?_,
              (by simpa [hTargetId]), rfl, hLineageId⟩
            · exact List.mem_map.mpr ⟨lookup, hTargetMem, by simp [hTargetId]⟩
      dsimp [ConcreteState.LineageAccounting,
        ConcreteState.tentativeLineages, ConcreteState.validatedFastLineages,
        ConcreteState.slowLineages, ConcreteState.totalTentativePhysicalLeases,
        ConcreteState.totalCommittedLineages] at hAccounting ⊢
      have hTentLen := length_tentative_updateFastLookupStage_tentative
        hFastUnique hLookup hObserved
      have hValLen := length_validated_updateFastLookupStage_tentative_eq
        hFastUnique hLookup hObserved
      have hTentUnit' : ∀ l ∈ ((c.lineages ++
          [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
            (fun (l : LeaseLineage) => match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false)), l.cloneCount = 1 := by
        intro l hMem
        have hKindTent : ∃ readerId, l.kind = .tentativeFast readerId := by
          cases hKind : l.kind with
          | tentativeFast readerId => exact ⟨readerId, by simpa using hKind⟩
          | validatedFast readerId =>
              have hKindFilter := (List.mem_filter.mp hMem).2
              simp [hKind] at hKindFilter
          | slow token =>
              have hKindFilter := (List.mem_filter.mp hMem).2
              simp [hKind] at hKindFilter
        exact hUnit' l (mem_of_mem_filter hMem) hKindTent
      have hTentPhys := sumCloneCounts_eq_length_of_unit hTentUnit'
      rcases hAccounting with ⟨hOldTent, hOldVal, hOldTentPhys, hOldCommitted⟩
      have hLineTentLen :
          ((c.lineages ++
            [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
              (fun (l : LeaseLineage) => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)).length =
            c.tentativeLineages.length + 1 := by
        simp [ConcreteState.tentativeLineages]
      have hLineValLen :
          ((c.lineages ++
            [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
              (fun (l : LeaseLineage) => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)).length =
            c.validatedFastLineages.length := by
        simp [ConcreteState.validatedFastLineages]
      have hLineTentPhys :
          sumCloneCounts ((c.lineages ++
            [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
              (fun (l : LeaseLineage) => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)) =
            sumCloneCounts c.tentativeLineages + 1 := by
        rw [show (c.lineages ++
          [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
            (fun (l : LeaseLineage) => match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false) =
            c.tentativeLineages ++
              [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)] by
          simp [ConcreteState.tentativeLineages]]
        rw [sumCloneCounts_append]
        simp [sumCloneCounts]
      have hLineCommitted :
          (((c.lineages ++
            [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
              (fun (l : LeaseLineage) => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)).length) +
            (((c.lineages ++
              [({ id := lineageId, kind := .tentativeFast readerId, cloneCount := 1 } : LeaseLineage)]).filter
                (fun (l : LeaseLineage) => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true)).length) =
            c.validatedFastLineages.length + c.slowLineages.length := by
        simp [ConcreteState.validatedFastLineages, ConcreteState.slowLineages]
      have hOldTent' : c.tentativeLineages.length = c.snapshot.tentativeFastLookups.length := by
        simpa [ConcreteState.tentativeLineages] using hOldTent
      have hOldVal' : c.validatedFastLineages.length = c.snapshot.validatedFastLookups.length := by
        simpa [ConcreteState.validatedFastLineages] using hOldVal
      have hOldTentPhys' : sumCloneCounts c.tentativeLineages = c.tentativeLineages.length := by
        simpa [ConcreteState.tentativeLineages] using hOldTentPhys
      have hOldCommitted' : c.totalCommittedLineages = c.snapshot.registry.activeLeases := by
        simpa [ConcreteState.totalCommittedLineages, ConcreteState.validatedFastLineages,
          ConcreteState.slowLineages] using hOldCommitted
      have hOldCommitted'' :
          c.validatedFastLineages.length + c.slowLineages.length =
            c.snapshot.registry.activeLeases := by
        simpa [ConcreteState.validatedFastLineages, ConcreteState.slowLineages]
          using hOldCommitted
      have hTentLen' :
          ((c.snapshot.updateFastLookupStage readerId .tentative).filter
            (fun x => decide (x.stage = .tentative))).length =
            c.snapshot.tentativeFastLookups.length + 1 := by
        change ((c.snapshot.fastLookups.map
          (fun x => if x.id = readerId then
            { x with stage := FastLookupStage.tentative } else x)).filter
          (fun x => decide (x.stage = FastLookupStage.tentative))).length =
          (c.snapshot.fastLookups.filter
            (fun x => decide (x.stage = FastLookupStage.tentative))).length + 1
        exact hTentLen
      have hValLen' :
          ((c.snapshot.updateFastLookupStage readerId .tentative).filter
            (fun x => decide (x.stage = .validated))).length =
            c.snapshot.validatedFastLookups.length := by
        change ((c.snapshot.fastLookups.map
          (fun x => if x.id = readerId then
            { x with stage := FastLookupStage.tentative } else x)).filter
          (fun x => decide (x.stage = FastLookupStage.validated))).length =
          (c.snapshot.fastLookups.filter
            (fun x => decide (x.stage = FastLookupStage.validated))).length
        exact hValLen
      refine ⟨?_, ?_, ?_, ?_⟩
      · rw [hLineTentLen, hOldTent']
        exact hTentLen'.symm
      · rw [hLineValLen, hOldVal']
        exact hValLen'.symm
      · rw [hLineTentPhys, hOldTentPhys', hLineTentLen]
      · rw [hLineCommitted, hOldCommitted'']

theorem lineage_eq_of_mem_id_eq
    {ls : List LeaseLineage} {lhs rhs : LeaseLineage}
    (hPair : ls.Pairwise (fun a b => a.id ≠ b.id))
    (hLhs : lhs ∈ ls) (hRhs : rhs ∈ ls)
    (hId : lhs.id = rhs.id) :
    lhs = rhs := by
  by_cases hEq : lhs = rhs
  · exact hEq
  · have hRel := pairwise_mem_ne_local hPair hLhs hRhs hEq
    rcases hRel with hRel | hRel
    · exact (hRel hId).elim
    · exact (hRel hId.symm).elim

theorem lineages_sound_update_clone
    {c : ConcreteState} {id newCount : Nat}
    (hSound : c.LineagesSound) :
    ∀ l ∈ c.updateLineageCloneCount id newCount,
      match l.kind with
      | .tentativeFast readerId =>
          ∃ lookup ∈ c.snapshot.fastLookups,
            lookup.id = readerId ∧ lookup.stage = .tentative ∧ l.id = readerId
      | .validatedFast readerId =>
          ∃ lookup ∈ c.snapshot.fastLookups,
            lookup.id = readerId ∧ lookup.stage = .validated ∧ l.id = readerId
      | .slow token => token.session = c.snapshot.registry.session := by
  intro l hMem
  rcases List.mem_map.mp hMem with ⟨orig, hOrig, hMap⟩
  have hKind : l.kind = orig.kind := by
    rw [← hMap]
    by_cases hId : orig.id = id <;> simp [hId]
  have hId : l.id = orig.id := by
    rw [← hMap]
    by_cases hId : orig.id = id <;> simp [hId]
  rw [hKind]
  cases hOrigKind : orig.kind with
  | tentativeFast readerId =>
      have hLine := hSound orig hOrig
      simp [hOrigKind] at hLine
      rcases hLine with ⟨lookup, hLookup, hLookupId, hStage, hOrigId⟩
      exact ⟨lookup, hLookup, by simpa [hKind] using hLookupId,
        by simpa [hKind] using hStage, hId.trans hOrigId⟩
  | validatedFast readerId =>
      have hLine := hSound orig hOrig
      simp [hOrigKind] at hLine
      rcases hLine with ⟨lookup, hLookup, hLookupId, hStage, hOrigId⟩
      exact ⟨lookup, hLookup, by simpa [hKind] using hLookupId,
        by simpa [hKind] using hStage, hId.trans hOrigId⟩
  | slow token =>
      have hToken := hSound orig hOrig
      simpa [hOrigKind] using hToken

theorem lineages_sound_remove_fast_lookup
    {c : ConcreteState} {readerId lineageId : Nat}
    (hSound : c.LineagesSound)
    (hLineageId : lineageId = readerId) :
    ∀ l ∈ c.removeLineage lineageId,
      match l.kind with
      | .tentativeFast oldReaderId =>
          ∃ lookup ∈ c.snapshot.removeFastLookup readerId,
            lookup.id = oldReaderId ∧ lookup.stage = .tentative ∧ l.id = oldReaderId
      | .validatedFast oldReaderId =>
          ∃ lookup ∈ c.snapshot.removeFastLookup readerId,
            lookup.id = oldReaderId ∧ lookup.stage = .validated ∧ l.id = oldReaderId
      | .slow token => token.session = c.snapshot.registry.session := by
  intro l hMem'
  have hOldMem := mem_of_mem_filter hMem'
  have hLineageNe := (List.mem_filter.mp hMem').2
  have hLineageNe' : l.id ≠ lineageId := bne_iff_ne.mp hLineageNe
  cases hKind : l.kind with
  | tentativeFast oldReaderId =>
      have hLine := hSound l hOldMem
      simp [hKind] at hLine
      rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
      have hReaderNe : oldLookup.id ≠ readerId := by
        intro hEq
        apply hLineageNe'
        calc
          l.id = oldReaderId := oldLineageId
          _ = oldLookup.id := oldId.symm
          _ = readerId := hEq
          _ = lineageId := hLineageId.symm
      exact ⟨oldLookup, List.mem_filter.mpr ⟨oldMem, bne_iff_ne.mpr hReaderNe⟩,
        oldId, oldStage, oldLineageId⟩
  | validatedFast oldReaderId =>
      have hLine := hSound l hOldMem
      simp [hKind] at hLine
      rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
      have hReaderNe : oldLookup.id ≠ readerId := by
        intro hEq
        apply hLineageNe'
        calc
          l.id = oldReaderId := oldLineageId
          _ = oldLookup.id := oldId.symm
          _ = readerId := hEq
          _ = lineageId := hLineageId.symm
      exact ⟨oldLookup, List.mem_filter.mpr ⟨oldMem, bne_iff_ne.mpr hReaderNe⟩,
        oldId, oldStage, oldLineageId⟩
  | slow token =>
      simpa [hKind] using hSound l hOldMem

theorem remove_validated_fast_lineage_invariant_preserved
    {c : ConcreteState} {s' : State} {readerId lineageId : Nat}
    {lineage : LeaseLineage} {lookup : FastLookup}
    (hInv : c.Invariant)
    (hMem : lineage ∈ c.lineages)
    (hId : lineage.id = lineageId)
    (hKind : lineage.kind = .validatedFast readerId)
    (hLineageId : lineageId = readerId)
    (hLookup : c.snapshot.findFastLookup? readerId = some lookup)
    (hValidated : lookup.stage = .validated)
    (hSnap' : s'.Invariant)
    (hSnapshot : s' = { c.snapshot with
      registry := { c.snapshot.registry with
        activeLeases := c.snapshot.registry.activeLeases - 1 }
      fastLookups := c.snapshot.removeFastLookup readerId }) :
    (({ snapshot := s', lineages := c.removeLineage lineageId } : ConcreteState).Invariant) := by
  subst s'
  rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
  have hUnique' : (c.removeLineage lineageId).Pairwise
      (fun lhs rhs => lhs.id ≠ rhs.id) := by
    dsimp [ConcreteState.removeLineage]
    exact pairwise_filter _ hUnique
  have hPositive' : ∀ l ∈ c.removeLineage lineageId, l.cloneCount > 0 := by
    intro l hMem'
    exact hPositive l (mem_of_mem_filter hMem')
  have hUnit' : ∀ l ∈ c.removeLineage lineageId,
      (∃ oldReaderId, l.kind = .tentativeFast oldReaderId) → l.cloneCount = 1 := by
    intro l hMem' hTentative
    exact hUnit l (mem_of_mem_filter hMem') hTentative
  have hSound' := lineages_sound_remove_fast_lookup hSound hLineageId
  have hTentList : (c.removeLineage lineageId).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false) = c.tentativeLineages := by
    rw [removeLineage_filter_eq (c := c) (id := lineageId)
      (p := fun l => match l.kind with
        | .tentativeFast _ => true
        | .validatedFast _ => false
        | .slow _ => false)]
    change (c.tentativeLineages.filter (fun l => l.id != lineageId)) =
      c.tentativeLineages
    apply List.filter_eq_self.mpr
    intro l hMemL
    have hTentative := (tentative_mem_kind (c := c) hMemL)
    have hMemLineages := mem_of_mem_filter hMemL
    have hKindL : ∃ oldReaderId, l.kind = .tentativeFast oldReaderId := by
      cases hKindL' : l.kind <;> simp [hKindL'] at hTentative ⊢
    have hLId : l.id ≠ lineageId := by
      intro hLId
      have hEq := lineage_eq_of_mem_id_eq hUnique hMemLineages hMem
        (hLId.trans hId.symm)
      subst l
      rcases hKindL with ⟨_, hKindL⟩
      rw [hKind] at hKindL
      cases hKindL
    exact bne_iff_ne.mpr hLId
  have hValList : (c.removeLineage lineageId).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false) =
      c.validatedFastLineages.filter (fun l => l.id != lineageId) := by
    exact removeLineage_filter_eq (c := c) (id := lineageId)
      (p := fun l => match l.kind with
        | .tentativeFast _ => false
        | .validatedFast _ => true
        | .slow _ => false)
  have hSlowList : (c.removeLineage lineageId).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => false
      | .slow _ => true) = c.slowLineages := by
    rw [removeLineage_filter_eq (c := c) (id := lineageId)
      (p := fun l => match l.kind with
        | .tentativeFast _ => false
        | .validatedFast _ => false
        | .slow _ => true)]
    change c.slowLineages.filter (fun l => l.id != lineageId) = c.slowLineages
    apply List.filter_eq_self.mpr
    intro l hMemL
    have hSlow := (slow_mem_kind (c := c) hMemL)
    have hMemLineages := mem_of_mem_filter hMemL
    have hKindL : ∃ token, l.kind = .slow token := by
      cases hKindL' : l.kind <;> simp [hKindL'] at hSlow ⊢
    have hLId : l.id ≠ lineageId := by
      intro hLId
      have hEq := lineage_eq_of_mem_id_eq hUnique hMemLineages hMem
        (hLId.trans hId.symm)
      subst l
      rcases hKindL with ⟨_, hKindL⟩
      rw [hKind] at hKindL
      cases hKindL
    exact bne_iff_ne.mpr hLId
  have hValMem : lineage ∈ c.validatedFastLineages := by
    apply List.mem_filter.mpr
    exact ⟨hMem, by simp [hKind]⟩
  have hValLineLen := length_lineage_filter_ne_of_mem
    (pairwise_filter (fun l => match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false) hUnique)
    hValMem hId
  have ⟨hTargetMem, hTargetId⟩ := findFastLookup?_mem_and_id hLookup
  have hValLookupMem : lookup ∈ c.snapshot.validatedFastLookups := by
    apply List.mem_filter.mpr
    exact ⟨hTargetMem, by simp [hValidated]⟩
  have hValSnapLenBase := length_filter_ne_of_mem
    (pairwise_filter (fun x => decide (x.stage = FastLookupStage.validated))
      hSnap.2.2.1)
    hValLookupMem
  have hValSnapLen :
      ({ c.snapshot with
        fastLookups := c.snapshot.removeFastLookup readerId }).validatedFastLookups.length + 1 =
        c.snapshot.validatedFastLookups.length := by
    dsimp [State.validatedFastLookups, State.removeFastLookup]
    rw [validated_removeFastLookup_filter_eq]
    simpa [hTargetId] using hValSnapLenBase
  have hTentSnapLen :
      ({ c.snapshot with
        fastLookups := c.snapshot.removeFastLookup readerId }).tentativeFastLookups.length =
        c.snapshot.tentativeFastLookups.length := by
    dsimp [State.tentativeFastLookups, State.removeFastLookup]
    rw [tentative_removeFastLookup_eq hSnap.2.2.1 hLookup (by simp [hValidated])]
  have hTentUnitAll : ∀ l ∈ (c.removeLineage lineageId).filter (fun l =>
      match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false), l.cloneCount = 1 := by
    intro l hMem'
    have hKindFilter := (List.mem_filter.mp hMem').2
    have hKindTent : ∃ oldReaderId, l.kind = .tentativeFast oldReaderId := by
      cases hKindL : l.kind with
      | tentativeFast oldReaderId => exact ⟨oldReaderId, by simpa using hKindL⟩
      | validatedFast oldReaderId => simp [hKindL] at hKindFilter
      | slow token => simp [hKindL] at hKindFilter
    exact hUnit' l (mem_of_mem_filter hMem') hKindTent
  have hTentPhys := sumCloneCounts_eq_length_of_unit hTentUnitAll
  have hOldTent := hAccounting.1
  have hOldVal := hAccounting.2.1
  have hOldTentPhys := hAccounting.2.2.1
  have hOldCommitted := hAccounting.2.2.2
  have hOldTent' : c.tentativeLineages.length = c.snapshot.tentativeFastLookups.length := by
    simpa [ConcreteState.tentativeLineages, State.tentativeFastLookups] using hOldTent
  have hOldVal' : c.validatedFastLineages.length = c.snapshot.validatedFastLookups.length := by
    simpa [ConcreteState.validatedFastLineages, State.validatedFastLookups] using hOldVal
  have hOldTentPhys' : sumCloneCounts c.tentativeLineages = c.tentativeLineages.length := by
    simpa [ConcreteState.totalTentativePhysicalLeases,
      ConcreteState.tentativeLineages] using hOldTentPhys
  have hOldCommitted' : c.validatedFastLineages.length + c.slowLineages.length =
      c.snapshot.registry.activeLeases := by
    simpa [ConcreteState.totalCommittedLineages,
      ConcreteState.validatedFastLineages, ConcreteState.slowLineages] using hOldCommitted
  refine ⟨hSnap', hUnique', hPositive', hUnit', hSound', ?_⟩
  change
    ((c.removeLineage lineageId).filter (fun l => match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false)).length =
        ({ c.snapshot with
          fastLookups := c.snapshot.removeFastLookup readerId }).tentativeFastLookups.length ∧
    ((c.removeLineage lineageId).filter (fun l => match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false)).length =
        ({ c.snapshot with
          fastLookups := c.snapshot.removeFastLookup readerId }).validatedFastLookups.length ∧
    sumCloneCounts ((c.removeLineage lineageId).filter (fun l => match l.kind with
      | .tentativeFast _ => true
      | .validatedFast _ => false
      | .slow _ => false)) =
        ((c.removeLineage lineageId).filter (fun l => match l.kind with
          | .tentativeFast _ => true
          | .validatedFast _ => false
          | .slow _ => false)).length ∧
    ((c.removeLineage lineageId).filter (fun l => match l.kind with
      | .tentativeFast _ => false
      | .validatedFast _ => true
      | .slow _ => false)).length +
        ((c.removeLineage lineageId).filter (fun l => match l.kind with
          | .tentativeFast _ => false
          | .validatedFast _ => false
          | .slow _ => true)).length =
      c.snapshot.registry.activeLeases - 1
  rw [hTentPhys, hTentList, hValList, hSlowList]
  have hValLen' :
      (c.validatedFastLineages.filter (fun l => l.id != lineageId)).length + 1 =
        c.validatedFastLineages.length := by
    simpa [ConcreteState.validatedFastLineages] using hValLineLen
  have hValSnapLen' :
      ((c.snapshot.removeFastLookup readerId).filter
        (fun x => decide (x.stage = FastLookupStage.validated))).length + 1 =
        c.snapshot.validatedFastLookups.length := by
    change ((c.snapshot.removeFastLookup readerId).filter
      (fun x => decide (x.stage = FastLookupStage.validated))).length + 1 =
      c.snapshot.validatedFastLookups.length
    exact hValSnapLen
  rw [hTentSnapLen]
  change
    c.tentativeLineages.length = c.snapshot.tentativeFastLookups.length ∧
    (c.validatedFastLineages.filter (fun l => l.id != lineageId)).length =
      ((c.snapshot.removeFastLookup readerId).filter
        (fun x => decide (x.stage = FastLookupStage.validated))).length ∧
    c.tentativeLineages.length = c.tentativeLineages.length ∧
    (c.validatedFastLineages.filter (fun l => l.id != lineageId)).length +
        c.slowLineages.length = c.snapshot.registry.activeLeases - 1
  omega

theorem concrete_step_invariant_preserved
    {c c' : ConcreteState} {event : ConcreteEvent}
    (hInv : c.Invariant)
    (hStep : ConcreteStep c event c') :
    c'.Invariant := by
  cases hStep with
  | baseStep hAbstract hNoLineageChange =>
      exact base_step_invariant_preserved hInv hAbstract hNoLineageChange
  | acquireTentativeLeaseLineage hNoLineage hLineageId hAbstract =>
      exact acquire_tentative_lease_invariant_preserved hInv hNoLineage hLineageId hAbstract rfl
  | cloneLineage hMem hId hNotTentative =>
      rename_i id lineage
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hUnique' := pairwise_updateLineageCloneCount
        (id := id) (newCount := lineage.cloneCount + 1) hUnique
      have hPositive' := positive_updateLineageCloneCount
        (id := id) (newCount := lineage.cloneCount + 1) hPositive (Nat.succ_pos _)
      have hUnit' : ∀ l ∈ c.updateLineageCloneCount id (lineage.cloneCount + 1),
          (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1 := by
        intro l hMem' hTentative
        rcases List.mem_map.mp hMem' with ⟨orig, hOrig, rfl⟩
        by_cases hOrigId : orig.id = id
        · have hEq := lineage_eq_of_mem_id_eq hUnique hOrig hMem
            (hOrigId.trans hId.symm)
          subst orig
          have hKindTent : ∃ readerId, lineage.kind = .tentativeFast readerId := by
            simpa [ConcreteState.updateLineageCloneCount, hId] using hTentative
          rcases hKindTent with ⟨readerId, hKindTent⟩
          exact (hNotTentative readerId hKindTent).elim
        · have hTentativeOrig : ∃ readerId, orig.kind = .tentativeFast readerId := by
            simpa [ConcreteState.updateLineageCloneCount, hOrigId] using hTentative
          simpa [hOrigId] using hUnit orig hOrig hTentativeOrig
      have hSound' := lineages_sound_update_clone
        (id := id) (newCount := lineage.cloneCount + 1) hSound
      have hTentFilter :
          (c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
            match l.kind with
            | .tentativeFast _ => true
            | .validatedFast _ => false
            | .slow _ => false) = c.tentativeLineages := by
        apply map_updateLineageCloneCount_filter_eq_of_target_false
          (p := fun l => match l.kind with
            | .tentativeFast _ => true
            | .validatedFast _ => false
            | .slow _ => false)
          (fun l => by cases l <;> rfl)
        intro l hLMem hLId
        have hEq := lineage_eq_of_mem_id_eq hUnique hLMem hMem
          (hLId.trans hId.symm)
        subst l
        cases hKind : lineage.kind with
        | tentativeFast readerId => exact (hNotTentative readerId hKind).elim
        | validatedFast readerId => simp [hKind]
        | slow token => simp [hKind]
      have hValLen := updateLineageCloneCount_validated_eq (c := c)
        (id := id) (newCount := lineage.cloneCount + 1)
      have hSlowLen := updateLineageCloneCount_slow_eq (c := c)
        (id := id) (newCount := lineage.cloneCount + 1)
      refine ⟨hSnap, hUnique', hPositive', hUnit', hSound', ?_⟩
      change
        ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => true
          | .validatedFast _ => false
          | .slow _ => false)).length = c.snapshot.tentativeFastLookups.length ∧
        ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => false
          | .validatedFast _ => true
          | .slow _ => false)).length = c.snapshot.validatedFastLookups.length ∧
        sumCloneCounts ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => true
          | .validatedFast _ => false
          | .slow _ => false)) =
            ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false)).length ∧
        ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => false
          | .validatedFast _ => true
          | .slow _ => false)).length +
          ((c.updateLineageCloneCount id (lineage.cloneCount + 1)).filter (fun l =>
            match l.kind with
            | .tentativeFast _ => false
            | .validatedFast _ => false
            | .slow _ => true)).length = c.snapshot.registry.activeLeases
      rw [hTentFilter, hValLen, hSlowLen]
      exact hAccounting
  | dropCloneNonFinal hMem hId hGtOne hNotTentative =>
      rename_i id lineage
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hUnique' := pairwise_updateLineageCloneCount
        (id := id) (newCount := lineage.cloneCount - 1) hUnique
      have hPositive' := positive_updateLineageCloneCount
        (id := id) (newCount := lineage.cloneCount - 1) hPositive (by omega)
      have hUnit' : ∀ l ∈ c.updateLineageCloneCount id (lineage.cloneCount - 1),
          (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1 := by
        intro l hMem' hTentative
        rcases List.mem_map.mp hMem' with ⟨orig, hOrig, rfl⟩
        by_cases hOrigId : orig.id = id
        · have hEq := lineage_eq_of_mem_id_eq hUnique hOrig hMem
            (hOrigId.trans hId.symm)
          subst orig
          have hKindTent : ∃ readerId, lineage.kind = .tentativeFast readerId := by
            simpa [ConcreteState.updateLineageCloneCount, hId] using hTentative
          rcases hKindTent with ⟨readerId, hKindTent⟩
          exact (hNotTentative readerId hKindTent).elim
        · have hTentativeOrig : ∃ readerId, orig.kind = .tentativeFast readerId := by
            simpa [ConcreteState.updateLineageCloneCount, hOrigId] using hTentative
          simpa [hOrigId] using hUnit orig hOrig hTentativeOrig
      have hSound' := lineages_sound_update_clone
        (id := id) (newCount := lineage.cloneCount - 1) hSound
      have hTentFilter :
          (c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
            match l.kind with
            | .tentativeFast _ => true
            | .validatedFast _ => false
            | .slow _ => false) = c.tentativeLineages := by
        apply map_updateLineageCloneCount_filter_eq_of_target_false
          (p := fun l => match l.kind with
            | .tentativeFast _ => true
            | .validatedFast _ => false
            | .slow _ => false)
          (fun l => by cases l <;> rfl)
        intro l hLMem hLId
        have hEq := lineage_eq_of_mem_id_eq hUnique hLMem hMem
          (hLId.trans hId.symm)
        subst l
        cases hKind : lineage.kind with
        | tentativeFast readerId => exact (hNotTentative readerId hKind).elim
        | validatedFast readerId => simp [hKind]
        | slow token => simp [hKind]
      have hValLen := updateLineageCloneCount_validated_eq (c := c)
        (id := id) (newCount := lineage.cloneCount - 1)
      have hSlowLen := updateLineageCloneCount_slow_eq (c := c)
        (id := id) (newCount := lineage.cloneCount - 1)
      refine ⟨hSnap, hUnique', hPositive', hUnit', hSound', ?_⟩
      change
        ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => true
          | .validatedFast _ => false
          | .slow _ => false)).length = c.snapshot.tentativeFastLookups.length ∧
        ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => false
          | .validatedFast _ => true
          | .slow _ => false)).length = c.snapshot.validatedFastLookups.length ∧
        sumCloneCounts ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => true
          | .validatedFast _ => false
          | .slow _ => false)) =
            ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false)).length ∧
        ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
          match l.kind with
          | .tentativeFast _ => false
          | .validatedFast _ => true
          | .slow _ => false)).length +
          ((c.updateLineageCloneCount id (lineage.cloneCount - 1)).filter (fun l =>
            match l.kind with
            | .tentativeFast _ => false
            | .validatedFast _ => false
            | .slow _ => true)).length = c.snapshot.registry.activeLeases
      rw [hTentFilter, hValLen, hSlowLen]
      exact hAccounting
  | rejectTentativeFastLookupLineage hMem hId hKind hLineageId hOne hAbstract =>
      rename_i readerId lineageId lineage
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hFastUnique := hSnap.2.2.1
      have hSnap' := Step.invariant_preserved hSnap hAbstract
      cases hAbstract with
      | rejectTentativeFastLookup hLookup hTentative hPub hNotLive =>
          have ⟨hTargetMem, hTargetId⟩ := findFastLookup?_mem_and_id hLookup
          have hUnique' : (c.removeLineage lineageId).Pairwise
              (fun lhs rhs => lhs.id ≠ rhs.id) := by
            dsimp [ConcreteState.removeLineage]
            exact pairwise_filter _ hUnique
          have hPositive' : ∀ l ∈ c.removeLineage lineageId, l.cloneCount > 0 := by
            intro l hMem'
            exact hPositive l (mem_of_mem_filter hMem')
          have hUnit' : ∀ l ∈ c.removeLineage lineageId,
              (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1 := by
            intro l hMem' hTentative'
            exact hUnit l (mem_of_mem_filter hMem') hTentative'
          have hSound' : ∀ l ∈ c.removeLineage lineageId,
              match l.kind with
              | .tentativeFast oldReaderId =>
                  ∃ lookup ∈ c.snapshot.removeFastLookup readerId,
                    lookup.id = oldReaderId ∧ lookup.stage = .tentative ∧ l.id = oldReaderId
              | .validatedFast oldReaderId =>
                  ∃ lookup ∈ c.snapshot.removeFastLookup readerId,
                    lookup.id = oldReaderId ∧ lookup.stage = .validated ∧ l.id = oldReaderId
              | .slow token => token.session = c.snapshot.registry.session := by
            intro l hMem'
            have hOldMem := mem_of_mem_filter hMem'
            cases hKind' : l.kind with
            | tentativeFast oldReaderId =>
                have hLine := hSound l hOldMem
                simp [hKind'] at hLine
                rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                have hLineageNe := (List.mem_filter.mp hMem').2
                have hLineageNe' : l.id ≠ lineageId := bne_iff_ne.mp hLineageNe
                have hReaderNe : oldLookup.id ≠ readerId := by
                  intro hEq
                  apply hLineageNe'
                  calc
                    l.id = oldReaderId := oldLineageId
                    _ = oldLookup.id := oldId.symm
                    _ = readerId := hEq
                    _ = lineageId := hLineageId.symm
                refine ⟨oldLookup, List.mem_filter.mpr ⟨oldMem, bne_iff_ne.mpr hReaderNe⟩,
                  oldId, oldStage, oldLineageId⟩
            | validatedFast oldReaderId =>
                have hLine := hSound l hOldMem
                simp [hKind'] at hLine
                rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                have hLineageNe := (List.mem_filter.mp hMem').2
                have hLineageNe' : l.id ≠ lineageId := bne_iff_ne.mp hLineageNe
                have hReaderNe : oldLookup.id ≠ readerId := by
                  intro hEq
                  apply hLineageNe'
                  calc
                    l.id = oldReaderId := oldLineageId
                    _ = oldLookup.id := oldId.symm
                    _ = readerId := hEq
                    _ = lineageId := hLineageId.symm
                refine ⟨oldLookup, List.mem_filter.mpr ⟨oldMem, bne_iff_ne.mpr hReaderNe⟩,
                  oldId, oldStage, oldLineageId⟩
            | slow token =>
                have hToken := hSound l hOldMem
                simpa [hKind'] using hToken
          have hTentList : (c.removeLineage lineageId).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false) =
              c.tentativeLineages.filter (fun l => l.id != lineageId) := by
            exact removeLineage_filter_eq (c := c) (id := lineageId)
              (p := fun l => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)
          have hValList : (c.removeLineage lineageId).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => false
              | .validatedFast _ => true
              | .slow _ => false) =
              c.validatedFastLineages := by
            rw [removeLineage_filter_eq (c := c) (id := lineageId)
              (p := fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)]
            change (c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)).filter (fun l => l.id != lineageId) =
              c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)
            calc
              (c.lineages.filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => true
                  | .slow _ => false)).filter (fun l => l.id != lineageId) =
                  (c.lineages.filter (fun l => l.id != lineageId)).filter
                    (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false) := by
                      simp [List.filter_filter, Bool.and_comm]
              _ = c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false) := by
                apply filter_id_ne_eq_of_no_id
                intro l hMemVal hVal
                have hKindVal : ∃ oldReaderId, l.kind = .validatedFast oldReaderId := by
                  cases hKindL : l.kind <;> simp [hKindL] at hVal ⊢
                intro hLId
                have hEq := lineage_eq_of_mem_id_eq hUnique hMemVal hMem
                  (hLId.trans hId.symm)
                subst l
                rcases hKindVal with ⟨_, hKindVal⟩
                rw [hKind] at hKindVal
                cases hKindVal
          have hSlowList : (c.removeLineage lineageId).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => false
              | .validatedFast _ => false
              | .slow _ => true) =
              c.slowLineages := by
            rw [removeLineage_filter_eq (c := c) (id := lineageId)
              (p := fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => false
                | .slow _ => true)]
            change (c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => false
                | .slow _ => true)).filter (fun l => l.id != lineageId) =
              c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => false
                | .slow _ => true)
            calc
              (c.lineages.filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true)).filter (fun l => l.id != lineageId) =
                  (c.lineages.filter (fun l => l.id != lineageId)).filter
                    (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => false
                    | .slow _ => true) := by
                      simp [List.filter_filter, Bool.and_comm]
              _ = c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => false
                    | .slow _ => true) := by
                apply filter_id_ne_eq_of_no_id
                intro l hMemSlow hSlow
                have hKindSlow : ∃ token, l.kind = .slow token := by
                  cases hKindL : l.kind <;> simp [hKindL] at hSlow ⊢
                intro hLId
                have hEq := lineage_eq_of_mem_id_eq hUnique hMemSlow hMem
                  (hLId.trans hId.symm)
                subst l
                rcases hKindSlow with ⟨_, hKindSlow⟩
                rw [hKind] at hKindSlow
                cases hKindSlow
          have hTentMem : lineage ∈ c.tentativeLineages := by
            apply List.mem_filter.mpr
            exact ⟨hMem, by simp [hKind]⟩
          have hPairLineages : c.lineages.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := hUnique
          have hPairTent := pairwise_filter (fun l => match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false) hPairLineages
          have hTentLineLen := length_lineage_filter_ne_of_mem
            hPairTent hTentMem hId
          have hTentSnapLen := length_tentative_removeFastLookup
            hFastUnique hLookup hTentative
          have hValSnapList := validated_removeFastLookup_eq hFastUnique hLookup (by simp [hTentative])
          have hOldAccounting := hAccounting
          have hTentUnitAll : ∀ l ∈ (c.removeLineage lineageId).filter (fun l =>
              match l.kind with
              | .tentativeFast _ => true
              | .validatedFast _ => false
              | .slow _ => false), l.cloneCount = 1 := by
            intro l hMemTent
            have hMemRemoved := mem_of_mem_filter hMemTent
            have hKindTent := (List.mem_filter.mp hMemTent).2
            have hKindExists : ∃ oldReaderId, l.kind = .tentativeFast oldReaderId := by
              cases hKind : l.kind <;> simp [hKind] at hKindTent ⊢
            exact hUnit' l hMemRemoved hKindExists
          have hTentUnitPhys := sumCloneCounts_eq_length_of_unit hTentUnitAll
          refine ⟨hSnap', hUnique', hPositive', hUnit', hSound', ?_⟩
          change
            List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)) =
                List.length ({ c.snapshot with fastLookups := c.snapshot.removeFastLookup readerId }.tentativeFastLookups) ∧
            List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)) =
                List.length ({ c.snapshot with fastLookups := c.snapshot.removeFastLookup readerId }.validatedFastLookups) ∧
            sumCloneCounts ((c.removeLineage lineageId).filter (fun l => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)) =
                List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => true
                  | .validatedFast _ => false
                  | .slow _ => false)) ∧
            List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)) +
                List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true)) =
              ({ c.snapshot with fastLookups := c.snapshot.removeFastLookup readerId }).registry.activeLeases
          dsimp [State.tentativeFastLookups, State.validatedFastLookups,
            State.removeFastLookup] at hTentSnapLen hValSnapList ⊢
          rw [hTentUnitPhys, hTentList, hValList, hSlowList,
            hValSnapList]
          dsimp [ConcreteState.tentativeLineages, ConcreteState.validatedFastLineages,
            ConcreteState.slowLineages] at hTentLineLen ⊢
          have hOldTent := hOldAccounting.1
          have hOldVal := hOldAccounting.2.1
          have hOldCommitted := hOldAccounting.2.2.2
          have hOldTent' :
              (c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false)).length =
                (c.snapshot.fastLookups.filter (fun x => decide (x.stage = .tentative))).length := by
            simpa [ConcreteState.tentativeLineages, State.tentativeFastLookups] using hOldTent
          have hOldVal' :
              (c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)).length =
                (c.snapshot.fastLookups.filter (fun x => decide (x.stage = .validated))).length := by
            simpa [ConcreteState.validatedFastLineages, State.validatedFastLookups] using hOldVal
          have hOldCommitted' :
              (c.lineages.filter (fun l => match l.kind with
                | .tentativeFast _ => false
                | .validatedFast _ => true
                | .slow _ => false)).length +
                (c.lineages.filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true)).length =
                c.snapshot.registry.activeLeases := by
            simpa [ConcreteState.totalCommittedLineages,
              ConcreteState.validatedFastLineages, ConcreteState.slowLineages] using hOldCommitted
          omega
  | validateFastLookupLineage hMem hId hKind hLineageId hAbstract =>
      rename_i readerId lineageId lineage
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hFastUnique := hSnap.2.2.1
      have hSnap' := Step.invariant_preserved hSnap hAbstract
      cases hAbstract with
      | validateFastLookup hLookup hTentative hPub hLive hReg =>
          cases hReg with
          | beginLookup hNotClosed hAuth hInBounds hLiveSlot =>
              have ⟨hTargetMem, hTargetId⟩ := findFastLookup?_mem_and_id hLookup
              have hUnique' : (c.updateLineageKind lineageId (.validatedFast readerId)).Pairwise
                  (fun lhs rhs => lhs.id ≠ rhs.id) := by
                exact pairwise_updateLineageKind hUnique
              have hPositive' := positive_updateLineageKind
                (id := lineageId) (kind := .validatedFast readerId) hPositive
              have hUnit' := unit_updateLineageKind
                (id := lineageId) (readerId := readerId) hUnit
              have hSound' : ∀ l ∈ c.updateLineageKind lineageId (.validatedFast readerId),
                  match l.kind with
                  | .tentativeFast oldReaderId =>
                      ∃ lookup ∈ ({ c.snapshot with
                        fastLookups := c.snapshot.updateFastLookupStage readerId .validated }).fastLookups,
                        lookup.id = oldReaderId ∧ lookup.stage = .tentative ∧ l.id = oldReaderId
                  | .validatedFast oldReaderId =>
                      ∃ lookup ∈ ({ c.snapshot with
                        fastLookups := c.snapshot.updateFastLookupStage readerId .validated }).fastLookups,
                        lookup.id = oldReaderId ∧ lookup.stage = .validated ∧ l.id = oldReaderId
                  | .slow token => token.session = c.snapshot.registry.session := by
                intro l hMem'
                rcases List.mem_map.mp hMem' with ⟨orig, hOrig, hMap⟩
                by_cases hOrigId : orig.id = lineageId
                · have hEq := lineage_eq_of_mem_id_eq hUnique hOrig hMem
                    (hOrigId.trans hId.symm)
                  subst orig
                  have hLine := hSound lineage hMem
                  simp [hKind] at hLine
                  rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                  rw [← hMap]
                  simp only [hId, if_pos]
                  refine ⟨{ oldLookup with stage := .validated }, ?_⟩
                  exact ⟨List.mem_map.mpr ⟨oldLookup, oldMem, by simp [oldId]⟩,
                    by simpa [oldId], rfl, by simp [hLineageId]⟩
                · cases hOrigKind : orig.kind with
                  | tentativeFast oldReaderId =>
                      have hLine := hSound orig hOrig
                      simp [hOrigKind] at hLine
                      rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                      have hReaderNe : oldLookup.id ≠ readerId := by
                        intro hEq
                        apply hOrigId
                        calc
                          orig.id = oldReaderId := oldLineageId
                          _ = oldLookup.id := oldId.symm
                          _ = readerId := hEq
                          _ = lineageId := hLineageId.symm
                      rw [← hMap]
                      simp [hOrigId, hOrigKind]
                      refine ⟨oldLookup, List.mem_map.mpr
                        ⟨oldLookup, oldMem, by simp [hReaderNe]⟩,
                        oldId, oldStage, oldLineageId⟩
                  | validatedFast oldReaderId =>
                      have hLine := hSound orig hOrig
                      simp [hOrigKind] at hLine
                      rcases hLine with ⟨oldLookup, oldMem, oldId, oldStage, oldLineageId⟩
                      have hReaderNe : oldLookup.id ≠ readerId := by
                        intro hEq
                        apply hOrigId
                        calc
                          orig.id = oldReaderId := oldLineageId
                          _ = oldLookup.id := oldId.symm
                          _ = readerId := hEq
                          _ = lineageId := hLineageId.symm
                      rw [← hMap]
                      simp [hOrigId, hOrigKind]
                      refine ⟨oldLookup, List.mem_map.mpr
                        ⟨oldLookup, oldMem, by simp [hReaderNe]⟩,
                        oldId, oldStage, oldLineageId⟩
                  | slow token =>
                      have hToken := hSound orig hOrig
                      rw [← hMap]
                      simpa [hOrigId, hOrigKind] using hToken
              have hTentList : (c.updateLineageKind lineageId (.validatedFast readerId)).filter
                  (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false) = c.tentativeLineages.filter (fun l => l.id != lineageId) := by
                change (c.lineages.map (fun l => if l.id = lineageId then
                  { l with kind := .validatedFast readerId } else l)).filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false) = c.tentativeLineages.filter (fun l => l.id != lineageId)
                have hRemoved := map_updateLineageKind_filter_eq_of_target_removed
                  (ls := c.lineages) (id := lineageId) (kind := .validatedFast readerId)
                  (p := fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)
                  (fun l hLMem hLId => by cases l <;> simp)
                simpa [ConcreteState.tentativeLineages, List.filter_filter, Bool.and_comm] using hRemoved
              have hValLen := map_updateLineageKind_filter_length_of_target_added
                (ls := c.lineages) (id := lineageId) (kind := .validatedFast readerId)
                (p := fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => true
                  | .slow _ => false)
                hUnique
                (fun l hLMem hLId => by cases l <;> simp)
                (fun l hLMem hLId => by
                  have hEq := lineage_eq_of_mem_id_eq hUnique hLMem hMem
                    (hLId.trans hId.symm)
                  subst l
                  simp [hKind])
                ⟨lineage, hMem, hId⟩
              have hSlowLen := map_updateLineageKind_filter_length_eq_of_target
                (ls := c.lineages) (id := lineageId) (kind := .validatedFast readerId)
                hUnique
                (p := fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true)
                (fun l hLMem hLId => by
                  have hEq := lineage_eq_of_mem_id_eq hUnique hLMem hMem
                    (hLId.trans hId.symm)
                  subst l
                  simp [hKind])
                ⟨lineage, hMem, hId⟩
              have hTentMem : lineage ∈ c.tentativeLineages := by
                apply List.mem_filter.mpr
                exact ⟨hMem, by simp [hKind]⟩
              have hPairTent := pairwise_filter (fun l => match l.kind with
                | .tentativeFast _ => true
                | .validatedFast _ => false
                | .slow _ => false) hUnique
              have hTentLineLen := length_lineage_filter_ne_of_mem
                hPairTent hTentMem hId
              have hTentSnapLen := length_tentative_updateFastLookupStage_validated
                hFastUnique hLookup hTentative
              have hValSnapLen := length_validated_updateFastLookupStage_validated
                hFastUnique hLookup hTentative
              have hTentUnitAll : ∀ l ∈ (c.updateLineageKind lineageId
                  (.validatedFast readerId)).filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false), l.cloneCount = 1 := by
                intro l hMemTent
                have hKindFilter := (List.mem_filter.mp hMemTent).2
                have hKindTent : ∃ oldReaderId, l.kind = .tentativeFast oldReaderId := by
                  cases hKindL : l.kind with
                  | tentativeFast oldReaderId => exact ⟨oldReaderId, by simpa using hKindL⟩
                  | validatedFast oldReaderId => simp [hKindL] at hKindFilter
                  | slow token => simp [hKindL] at hKindFilter
                exact hUnit' l (mem_of_mem_filter hMemTent) hKindTent
              have hTentPhys := sumCloneCounts_eq_length_of_unit hTentUnitAll
              rcases hAccounting with ⟨hOldTent, hOldVal, hOldTentPhys, hOldCommitted⟩
              have hOldTent' : c.tentativeLineages.length =
                  c.snapshot.tentativeFastLookups.length := hOldTent
              have hOldVal' : c.validatedFastLineages.length =
                  c.snapshot.validatedFastLookups.length := hOldVal
              have hOldTentPhys' : sumCloneCounts c.tentativeLineages =
                  c.tentativeLineages.length := hOldTentPhys
              have hOldCommitted' : c.validatedFastLineages.length + c.slowLineages.length =
                  c.snapshot.registry.activeLeases := hOldCommitted
              refine ⟨hSnap', hUnique', hPositive', hUnit', hSound', ?_⟩
              dsimp [ConcreteState.LineageAccounting, ConcreteState.tentativeLineages,
                ConcreteState.validatedFastLineages, ConcreteState.slowLineages,
                ConcreteState.totalTentativePhysicalLeases,
                ConcreteState.totalCommittedLineages,
                State.tentativeFastLookups, State.validatedFastLookups] at ⊢
              dsimp [ConcreteState.updateLineageKind] at ⊢
              have hTentList' := hTentList
              dsimp [ConcreteState.updateLineageKind, ConcreteState.tentativeLineages] at hTentList'
              have hValLen' := hValLen
              have hSlowLen' := hSlowLen
              have hTentPhys' := hTentPhys
              dsimp [ConcreteState.updateLineageKind] at hValLen' hSlowLen' hTentPhys'
              have hTentSnapLen' :
                  ((c.snapshot.updateFastLookupStage readerId .validated).filter
                      (fun x => decide (x.stage = FastLookupStage.tentative))).length + 1 =
                    c.snapshot.tentativeFastLookups.length := by
                change ((c.snapshot.fastLookups.map
                  (fun x => if x.id = readerId then
                    { x with stage := FastLookupStage.validated } else x)).filter
                  (fun x => decide (x.stage = FastLookupStage.tentative))).length + 1 =
                  (c.snapshot.fastLookups.filter
                    (fun x => decide (x.stage = FastLookupStage.tentative))).length
                exact hTentSnapLen
              have hValSnapLen' :
                  ((c.snapshot.updateFastLookupStage readerId .validated).filter
                      (fun x => decide (x.stage = FastLookupStage.validated))).length =
                    c.snapshot.validatedFastLookups.length + 1 := by
                change ((c.snapshot.fastLookups.map
                  (fun x => if x.id = readerId then
                    { x with stage := FastLookupStage.validated } else x)).filter
                  (fun x => decide (x.stage = FastLookupStage.validated))).length =
                  (c.snapshot.fastLookups.filter
                    (fun x => decide (x.stage = FastLookupStage.validated))).length + 1
                exact hValSnapLen
              have hTentLineLen' :
                  (c.tentativeLineages.filter (fun l => l.id != lineageId)).length + 1 =
                    c.tentativeLineages.length := by
                simpa [ConcreteState.tentativeLineages] using hTentLineLen
              rw [hValLen', hSlowLen', hTentPhys', hTentList']
              change
                (c.tentativeLineages.filter (fun l => l.id != lineageId)).length =
                    ((c.snapshot.updateFastLookupStage readerId .validated).filter
                      (fun x => decide (x.stage = FastLookupStage.tentative))).length ∧
                c.validatedFastLineages.length + 1 =
                    ((c.snapshot.updateFastLookupStage readerId .validated).filter
                      (fun x => decide (x.stage = FastLookupStage.validated))).length ∧
                (c.tentativeLineages.filter (fun l => l.id != lineageId)).length =
                    (c.tentativeLineages.filter (fun l => l.id != lineageId)).length ∧
                c.validatedFastLineages.length + 1 + c.slowLineages.length =
                    c.snapshot.registry.activeLeases + 1
              constructor
              · omega
              constructor
              · omega
              constructor
              · rfl
              · omega
  | fallbackFastLookupLineage hMem hId hKind hLineageId hOne hAbstract =>
      rename_i readerId lineageId lineage
      have hSnap' := Step.invariant_preserved hInv.1 hAbstract
      cases hAbstract with
      | fallbackFastLookup hLookup hValidated hPub hNotLive hReg =>
          cases hReg with
          | endLookup hLeases =>
              exact remove_validated_fast_lineage_invariant_preserved
                hInv hMem hId hKind hLineageId hLookup hValidated hSnap' rfl
  | beginSlowLookupLineage hNoLineage hAbstract =>
      rename_i token lineageId
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hSnap' := Step.invariant_preserved hSnap hAbstract
      cases hAbstract with
      | beginSlowLookup hNotSealed hReg =>
          cases hReg with
          | beginLookup hNotClosed hAuth hInBounds hLive =>
              let newLineage : LeaseLineage :=
                { id := lineageId, kind := .slow token, cloneCount := 1 }
              have hUnique' : (c.lineages ++
                  [newLineage]).Pairwise
                    (fun lhs rhs => lhs.id ≠ rhs.id) := by
                apply pairwise_append_singleton hUnique
                exact hNoLineage
              have hPositive' : ∀ l ∈ c.lineages ++ [newLineage],
                  l.cloneCount > 0 := by
                intro l hMem
                simp only [List.mem_append, List.mem_singleton] at hMem
                cases hMem with
                | inl hOld => exact hPositive l hOld
                | inr hNew =>
                    subst l
                    simp [newLineage]
              have hUnit' : ∀ l ∈ c.lineages ++ [newLineage],
                  (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1 := by
                intro l hMem hTentative
                simp only [List.mem_append, List.mem_singleton] at hMem
                cases hMem with
                | inl hOld => exact hUnit l hOld hTentative
                | inr hNew =>
                    subst l
                    simp [newLineage] at hTentative
              have hSound' : ∀ l ∈ c.lineages ++ [newLineage],
                  match l.kind with
                  | .tentativeFast readerId =>
                      ∃ lookup ∈ c.snapshot.fastLookups,
                        lookup.id = readerId ∧ lookup.stage = .tentative ∧ l.id = readerId
                  | .validatedFast readerId =>
                      ∃ lookup ∈ c.snapshot.fastLookups,
                        lookup.id = readerId ∧ lookup.stage = .validated ∧ l.id = readerId
                  | .slow token => token.session = c.snapshot.registry.session := by
                intro l hMem
                simp only [List.mem_append, List.mem_singleton] at hMem
                cases hMem with
                | inl hOld => exact hSound l hOld
                | inr hNew =>
                    subst l
                    simpa [newLineage, State.AuthenticatedFor] using hAuth
              refine ⟨hSnap', hUnique', hPositive', hUnit', hSound', ?_⟩
              dsimp [ConcreteState.LineageAccounting, ConcreteState.tentativeLineages,
                ConcreteState.validatedFastLineages, ConcreteState.totalTentativePhysicalLeases,
                ConcreteState.totalCommittedLineages, State.tentativeFastLookups,
                State.validatedFastLookups]
              rcases hAccounting with ⟨hTent, hVal, hTentPhys, hCommitted⟩
              simp only [List.filter_append]
              simp [ConcreteState.tentativeLineages, ConcreteState.validatedFastLineages,
                ConcreteState.slowLineages, ConcreteState.totalCommittedLineages, newLineage]
              have hTent' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)).length =
                    (c.snapshot.fastLookups.filter (fun x => decide (x.stage = .tentative))).length := by
                simpa [ConcreteState.tentativeLineages, State.tentativeFastLookups] using hTent
              have hVal' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)).length =
                    (c.snapshot.fastLookups.filter (fun x => decide (x.stage = .validated))).length := by
                simpa [ConcreteState.validatedFastLineages, State.validatedFastLookups] using hVal
              have hTentPhys' :
                  sumCloneCounts (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)) =
                    (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => true
                      | .validatedFast _ => false
                      | .slow _ => false)).length := by
                simpa [ConcreteState.totalTentativePhysicalLeases,
                  ConcreteState.tentativeLineages] using hTentPhys
              have hCommitted' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)).length +
                    (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => false
                      | .validatedFast _ => false
                      | .slow _ => true)).length =
                    c.snapshot.registry.activeLeases := by
                simpa [ConcreteState.totalCommittedLineages,
                  ConcreteState.validatedFastLineages, ConcreteState.slowLineages] using hCommitted
              omega
  | dropCloneFinalFast hMem hId hKind hLineageId hOne hAbstract =>
      rename_i readerId lineageId lineage
      have hSnap' := Step.invariant_preserved hInv.1 hAbstract
      cases hAbstract with
      | completeFastLookup hLookup hValidated hReg =>
          cases hReg with
          | endLookup hLeases =>
              exact remove_validated_fast_lineage_invariant_preserved
                hInv hMem hId hKind hLineageId hLookup hValidated hSnap' rfl
  | dropCloneFinalSlow hMem hId hKind hOne hAbstract =>
      rename_i token lineageId lineage
      rcases hInv with ⟨hSnap, hUnique, hPositive, hUnit, hSound, hAccounting⟩
      have hSnap' := Step.invariant_preserved hSnap hAbstract
      cases hAbstract with
      | endSlowLookup hSlowLease hReg =>
          cases hReg with
          | endLookup hLeases =>
              have hUnique' : (c.removeLineage lineageId).Pairwise
                  (fun lhs rhs => lhs.id ≠ rhs.id) := by
                dsimp [ConcreteState.removeLineage]
                exact pairwise_filter _ hUnique
              have hPositive' : ∀ l ∈ c.removeLineage lineageId, l.cloneCount > 0 := by
                intro l hMem'
                exact hPositive l (mem_of_mem_filter hMem')
              have hUnit' : ∀ l ∈ c.removeLineage lineageId,
                  (∃ readerId, l.kind = .tentativeFast readerId) → l.cloneCount = 1 := by
                intro l hMem' hTentative
                exact hUnit l (mem_of_mem_filter hMem') hTentative
              have hSound' : ∀ l ∈ c.removeLineage lineageId,
                  match l.kind with
                  | .tentativeFast readerId =>
                      ∃ lookup ∈ c.snapshot.fastLookups,
                        lookup.id = readerId ∧ lookup.stage = .tentative ∧ l.id = readerId
                  | .validatedFast readerId =>
                      ∃ lookup ∈ c.snapshot.fastLookups,
                        lookup.id = readerId ∧ lookup.stage = .validated ∧ l.id = readerId
                  | .slow token => token.session = c.snapshot.registry.session := by
                intro l hMem'
                exact hSound l (mem_of_mem_filter hMem')
              have hTentList : (c.removeLineage lineageId).filter (fun l =>
                  match l.kind with
                  | .tentativeFast _ => true
                  | .validatedFast _ => false
                  | .slow _ => false) = c.tentativeLineages := by
                rw [removeLineage_filter_eq (c := c) (id := lineageId)
                  (p := fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)]
                change (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)).filter (fun l => l.id != lineageId) =
                  c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)
                calc
                  (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => true
                      | .validatedFast _ => false
                      | .slow _ => false)).filter (fun l => l.id != lineageId) =
                      (c.lineages.filter (fun l => l.id != lineageId)).filter
                        (fun l => match l.kind with
                        | .tentativeFast _ => true
                        | .validatedFast _ => false
                        | .slow _ => false) := by
                          simp [List.filter_filter, Bool.and_comm]
                  _ = c.lineages.filter (fun l => match l.kind with
                        | .tentativeFast _ => true
                        | .validatedFast _ => false
                        | .slow _ => false) := by
                    apply filter_id_ne_eq_of_no_id
                    intro l hMemL hTentative
                    have hKindL : ∃ oldReaderId, l.kind = .tentativeFast oldReaderId := by
                      cases hKindL' : l.kind <;> simp [hKindL'] at hTentative ⊢
                    intro hLId
                    have hEq := lineage_eq_of_mem_id_eq hUnique hMemL hMem
                      (hLId.trans hId.symm)
                    subst l
                    rcases hKindL with ⟨_, hKindL⟩
                    rw [hKind] at hKindL
                    cases hKindL
              have hValList : (c.removeLineage lineageId).filter (fun l =>
                  match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => true
                  | .slow _ => false) = c.validatedFastLineages := by
                rw [removeLineage_filter_eq (c := c) (id := lineageId)
                  (p := fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)]
                change (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)).filter (fun l => l.id != lineageId) =
                  c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)
                calc
                  (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => false
                      | .validatedFast _ => true
                      | .slow _ => false)).filter (fun l => l.id != lineageId) =
                      (c.lineages.filter (fun l => l.id != lineageId)).filter
                        (fun l => match l.kind with
                        | .tentativeFast _ => false
                        | .validatedFast _ => true
                        | .slow _ => false) := by
                          simp [List.filter_filter, Bool.and_comm]
                  _ = c.lineages.filter (fun l => match l.kind with
                        | .tentativeFast _ => false
                        | .validatedFast _ => true
                        | .slow _ => false) := by
                    apply filter_id_ne_eq_of_no_id
                    intro l hMemL hValidated
                    have hKindL : ∃ oldReaderId, l.kind = .validatedFast oldReaderId := by
                      cases hKindL' : l.kind <;> simp [hKindL'] at hValidated ⊢
                    intro hLId
                    have hEq := lineage_eq_of_mem_id_eq hUnique hMemL hMem
                      (hLId.trans hId.symm)
                    subst l
                    rcases hKindL with ⟨_, hKindL⟩
                    rw [hKind] at hKindL
                    cases hKindL
              have hSlowMem : lineage ∈ c.slowLineages := by
                apply List.mem_filter.mpr
                exact ⟨hMem, by simp [hKind]⟩
              have hSlowLen := length_lineage_filter_ne_of_mem
                (pairwise_filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true) hUnique)
                hSlowMem hId
              have hSlowList : (c.removeLineage lineageId).filter (fun l =>
                  match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => false
                  | .slow _ => true) =
                  c.slowLineages.filter (fun l => l.id != lineageId) := by
                exact removeLineage_filter_eq (c := c) (id := lineageId)
                  (p := fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => false
                    | .slow _ => true)
              have hTentPhys := hAccounting.2.2.1
              have hOldTent := hAccounting.1
              have hOldVal := hAccounting.2.1
              have hOldCommitted := hAccounting.2.2.2
              refine ⟨hSnap', hUnique', hPositive', hUnit', hSound', ?_⟩
              change
                List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => true
                  | .validatedFast _ => false
                  | .slow _ => false)) = c.snapshot.tentativeFastLookups.length ∧
                List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => true
                  | .slow _ => false)) = c.snapshot.validatedFastLookups.length ∧
                sumCloneCounts ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => true
                  | .validatedFast _ => false
                  | .slow _ => false)) =
                  List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)) ∧
                List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                  | .tentativeFast _ => false
                  | .validatedFast _ => true
                  | .slow _ => false)) +
                  List.length ((c.removeLineage lineageId).filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => false
                    | .slow _ => true)) =
                  (c.snapshot.registry.activeLeases - 1)
              rw [hTentList, hValList, hSlowList]
              dsimp [ConcreteState.tentativeLineages, ConcreteState.validatedFastLineages,
                ConcreteState.slowLineages, State.tentativeFastLookups,
                State.validatedFastLookups]
              have hOldTent' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)).length = c.snapshot.tentativeFastLookups.length := by
                simpa [ConcreteState.tentativeLineages, State.tentativeFastLookups] using hOldTent
              have hOldVal' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)).length = c.snapshot.validatedFastLookups.length := by
                simpa [ConcreteState.validatedFastLineages, State.validatedFastLookups] using hOldVal
              have hTentPhys' :
                  sumCloneCounts (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)) =
                    (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => true
                      | .validatedFast _ => false
                      | .slow _ => false)).length := by
                simpa [ConcreteState.totalTentativePhysicalLeases,
                  ConcreteState.tentativeLineages] using hTentPhys
              have hSlowLen' :
                  List.length ((c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => false
                    | .slow _ => true)).filter (fun l => l.id != lineageId)) + 1 =
                    List.length (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => false
                      | .validatedFast _ => false
                      | .slow _ => true)) := by
                simpa [ConcreteState.slowLineages] using hSlowLen
              have hCommitted' :
                  (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => false
                    | .validatedFast _ => true
                    | .slow _ => false)).length +
                    (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => false
                      | .validatedFast _ => false
                      | .slow _ => true)).length =
                    c.snapshot.registry.activeLeases := by
                simpa [ConcreteState.totalCommittedLineages,
                  ConcreteState.validatedFastLineages, ConcreteState.slowLineages] using hOldCommitted
              have hTentPhys' :
                  sumCloneCounts (c.lineages.filter (fun l => match l.kind with
                    | .tentativeFast _ => true
                    | .validatedFast _ => false
                    | .slow _ => false)) =
                    (c.lineages.filter (fun l => match l.kind with
                      | .tentativeFast _ => true
                      | .validatedFast _ => false
                      | .slow _ => false)).length := by
                simpa [ConcreteState.totalTentativePhysicalLeases,
                  ConcreteState.tentativeLineages] using hTentPhys
              refine ⟨hOldTent', hOldVal', hTentPhys', ?_⟩
              omega

theorem ConcreteStep.invariant_preserved
    {c c' : ConcreteState} {event : ConcreteEvent}
    (hInv : c.Invariant)
    (hStep : ConcreteStep c event c') :
    c'.Invariant := by
  exact concrete_step_invariant_preserved hInv hStep

def ConcreteEvent.project : ConcreteEvent → Option Event
  | .baseStep event => some event
  | .cloneLineage _ | .dropCloneNonFinal _ => none
  | .dropCloneFinalFast readerId _ => some (.completeFastLookup readerId)
  | .dropCloneFinalSlow _ => some .endSlowLookup

theorem concrete_step_projects
    {c c' : ConcreteState} {event : ConcreteEvent}
    (hStep : ConcreteStep c event c') :
    match event.project with
    | none => c'.snapshot = c.snapshot
    | some abstractEvent =>
        ∃ snapshot', Step c.snapshot abstractEvent snapshot' ∧
          c'.snapshot = snapshot' := by
  cases hStep with
  | baseStep hAbstract hNoLineageChange =>
      exact ⟨_, hAbstract, rfl⟩
  | acquireTentativeLeaseLineage hNoLineage hLineageId hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | rejectTentativeFastLookupLineage hMem hId hKind hLineageId hOne hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | validateFastLookupLineage hMem hId hKind hLineageId hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | fallbackFastLookupLineage hMem hId hKind hLineageId hOne hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | beginSlowLookupLineage hNoLineage hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | cloneLineage hMem hId hNotTentative =>
      rfl
  | dropCloneNonFinal hMem hId hGtOne hNotTentative =>
      rfl
  | dropCloneFinalFast hMem hId hKind hLineageId hOne hAbstract =>
      exact ⟨_, hAbstract, rfl⟩
  | dropCloneFinalSlow hMem hId hKind hOne hAbstract =>
      exact ⟨_, hAbstract, rfl⟩

inductive ConcreteReachable : ConcreteState → ConcreteState → Prop where
  | refl (c : ConcreteState) : ConcreteReachable c c
  | tail {c c' c'' : ConcreteState} {event : ConcreteEvent} :
      ConcreteReachable c c' → ConcreteStep c' event c'' →
      ConcreteReachable c c''

theorem ConcreteReachable.invariant_preserved
    {c c' : ConcreteState}
    (hInv : c.Invariant)
    (hReach : ConcreteReachable c c') :
    c'.Invariant := by
  induction hReach with
  | refl =>
      exact hInv
  | tail hPrefix hStep ih =>
      exact ConcreteStep.invariant_preserved ih hStep

end XlFnFormal.Handle.Registry.Snapshot
