import XlFnFormal.Handle.Registry.Snapshot.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

theorem mem_of_mem_filter {α : Type} {p : α → Bool} {x : α} {l : List α}
    (h : x ∈ l.filter p) : x ∈ l := by
  induction l with
  | nil => contradiction
  | cons y ys ih =>
      dsimp [List.filter] at h
      split at h
      · cases List.mem_cons.mp h with
        | inl h1 => subst h1; exact List.mem_cons_self
        | inr h2 => exact List.mem_cons_of_mem y (ih h2)
      · exact List.mem_cons_of_mem y (ih h)

theorem pairwise_filter {α : Type} {R : α → α → Prop} (p : α → Bool)
    {l : List α} (h : l.Pairwise R) : (l.filter p).Pairwise R := by
  induction h with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      dsimp [List.filter]
      split
      · refine List.Pairwise.cons ?_ ih
        intro x hx
        exact hHead x (mem_of_mem_filter hx)
      · exact ih

theorem pairwise_append_singleton
    {α : Type} {R : α → α → Prop} {l : List α} {x : α}
    (hPair : l.Pairwise R)
    (hSep : ∀ y ∈ l, R y x) :
    (l ++ [x]).Pairwise R := by
  rw [List.pairwise_append]
  refine ⟨hPair,
    List.Pairwise.cons (fun y hy => False.elim (List.not_mem_nil hy)) List.Pairwise.nil,
    ?_⟩
  intro y hy z hz
  simp only [List.mem_singleton] at hz
  subst z
  exact hSep y hy

theorem pairwise_map
    {α : Type} {R : α → α → Prop} {l : List α} {f : α → α}
    (hPair : l.Pairwise R)
    (hRel : ∀ a ∈ l, ∀ b ∈ l, R a b → R (f a) (f b)) :
    (l.map f).Pairwise R := by
  induction hPair with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      simp only [List.map]
      refine List.Pairwise.cons ?_ (ih (fun a hA b hB hR =>
        hRel a (List.mem_cons_of_mem _ hA) b (List.mem_cons_of_mem _ hB) hR))
      intro x hx
      rcases List.mem_map.mp hx with ⟨y, hy, rfl⟩
      exact hRel _ List.mem_cons_self _ (List.mem_cons_of_mem _ hy) (hHead y hy)

theorem length_filter_ne_of_mem
    {l : List FastLookup} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hMem : lookup ∈ l) :
    (l.filter (fun x => x.id != lookup.id)).length + 1 = l.length := by
  induction l with
  | nil => contradiction
  | cons head tail ih =>
      cases hPair with
      | cons hHead hTail =>
          dsimp [List.filter]
          cases List.mem_cons.mp hMem with
          | inl hEq =>
              subst hEq
              simp only [bne_self_eq_false]
              have hFilterId : tail.filter (fun x => x.id != lookup.id) = tail := by
                apply List.filter_eq_self.mpr
                intro x hx
                have hNe := hHead x hx
                exact bne_iff_ne.mpr (Ne.symm hNe)
              rw [hFilterId]
          | inr hInTail =>
              have hNe : head.id ≠ lookup.id := by
                intro hEq
                exact (hHead lookup hInTail) hEq
              have hBne : (head.id != lookup.id) = true := bne_iff_ne.mpr hNe
              rw [hBne]
              simp only [List.length_cons]
              have ihRes := ih hTail hInTail
              omega

theorem findFastLookup?_mem_and_id
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hSome : l.find? (fun x => x.id == id) = some lookup) :
    lookup ∈ l ∧ lookup.id = id := by
  have hMem := List.mem_of_find?_eq_some hSome
  have hProp := List.find?_some hSome
  have hId : lookup.id = id := by
    cases h : lookup.id == id
    · rw [h] at hProp; contradiction
    · exact beq_iff_eq.mp h
  exact ⟨hMem, hId⟩

theorem findPublication?_none
    {l : List Publication} {slot : SlotId} {gen : Generation}
    (hNone : l.find? (fun p => p.slot == slot && p.generation == gen) = none) :
    ∀ p ∈ l, ¬ (p.slot = slot ∧ p.generation = gen) := by
  intro p hp ⟨hS, hG⟩
  have hFind := List.find?_eq_none.mp hNone p hp
  subst hS hG
  simp at hFind

theorem findPublication?_some_prop
    {l : List Publication} {slot : SlotId} {gen : Generation} {pub : Publication}
    (hSome : l.find? (fun p => p.slot == slot && p.generation == gen) = some pub) :
    pub.slot = slot ∧ pub.generation = gen := by
  have hProp := List.find?_some hSome
  have hBool : (pub.slot == slot && pub.generation == gen) = true := hProp
  rw [Bool.and_eq_true] at hBool
  exact ⟨beq_iff_eq.mp hBool.1, beq_iff_eq.mp hBool.2⟩

theorem findSnapshot?_none
    {l : List SnapshotBinding} {slot : SlotId}
    (hNone : l.find? (fun b => b.slot == slot) = none) :
    ∀ b ∈ l, b.slot ≠ slot := by
  intro b hb hS
  have hFind := List.find?_eq_none.mp hNone b hb
  subst hS
  simp at hFind

theorem findFastLookup?_none
    {l : List FastLookup} {id : Nat}
    (hNone : l.find? (fun l => l.id == id) = none) :
    ∀ l' ∈ l, l'.id ≠ id := by
  intro l' hl hId
  have hFind := List.find?_eq_none.mp hNone l' hl
  subst hId
  simp at hFind

theorem pairwise_updateFastLookupStage
    {l : List FastLookup} {id : Nat} {stage : FastLookupStage}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)) :
    (l.map (fun lookup => if lookup.id = id then { lookup with stage := stage } else lookup)).Pairwise
      (fun lhs rhs => lhs.id ≠ rhs.id) := by
  apply pairwise_map hPair
  intro a _ b _ hR
  split <;> split <;> exact hR

theorem validated_filter_append_observed
    {l : List FastLookup} {lookup : FastLookup}
    (hStage : lookup.stage = .observed) :
    (l ++ [lookup]).filter (fun x => decide (x.stage = .validated)) =
      l.filter (fun x => decide (x.stage = .validated)) := by
  simp [hStage]

theorem map_updateFastLookupStage_eq_self_of_id_ne
    {l : List FastLookup} {id : Nat} {stage : FastLookupStage}
    (hNe : ∀ lookup ∈ l, lookup.id ≠ id) :
    l.map (fun lookup => if lookup.id = id then { lookup with stage := stage } else lookup) = l := by
  induction l with
  | nil => rfl
  | cons head tail ih =>
      have hHead := hNe head List.mem_cons_self
      have hTail : ∀ lookup ∈ tail, lookup.id ≠ id := by
        intro lookup hMem
        exact hNe lookup (List.mem_cons_of_mem head hMem)
      have hHeadUpdate :
          (if head.id = id then { head with stage := stage } else head) = head := by
        simp [hHead]
      simp only [List.map]
      rw [hHeadUpdate, ih hTail]

theorem length_validated_updateFastLookupStage_validated
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hTentative : lookup.stage = .tentative) :
    ((l.map (fun x => if x.id = id then { x with stage := .validated } else x)).filter
        (fun x => decide (x.stage = .validated))).length =
      (l.filter (fun x => decide (x.stage = .validated))).length + 1 := by
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
            have hIdNe : (head.id != id) = false := by simp [hHeadId]
            have hTailMap := map_updateFastLookupStage_eq_self_of_id_ne
              (l := tail) (id := id) (stage := FastLookupStage.validated) hNoTail
            have hOldHead : decide (head.stage = FastLookupStage.validated) = false := by
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
            by_cases hStage : decide (head.stage = FastLookupStage.validated) = true
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult

theorem length_validated_updateFastLookupStage_tentative_eq
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hObserved : lookup.stage = .observed) :
    ((l.map (fun x => if x.id = id then { x with stage := .tentative } else x)).filter
        (fun x => decide (x.stage = .validated))).length =
      (l.filter (fun x => decide (x.stage = .validated))).length := by
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
            have hOldHead : decide (head.stage = FastLookupStage.validated) = false := by
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
            by_cases hStage : decide (head.stage = FastLookupStage.validated) = true
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult
            · simpa [List.map, List.filter, hIdFalse, hStage] using hTailResult

theorem filter_validated_and_id_ne_eq
    {l : List FastLookup} {id : Nat}
    (hNe : ∀ lookup ∈ l, lookup.id ≠ id) :
    l.filter (fun x => decide (x.stage = .validated) && x.id != id) =
      l.filter (fun x => decide (x.stage = .validated)) := by
  induction l with
  | nil => rfl
  | cons head tail ih =>
      have hHeadNe := hNe head List.mem_cons_self
      have hTailNe : ∀ lookup ∈ tail, lookup.id ≠ id := by
        intro lookup hMem
        exact hNe lookup (List.mem_cons_of_mem head hMem)
      have hIdNe : (head.id != id) = true := bne_iff_ne.mpr hHeadNe
      by_cases hStage : decide (head.stage = FastLookupStage.validated) = true
      · simp [List.filter, hIdNe, hStage, ih hTailNe]
      · simp [List.filter, hIdNe, hStage, ih hTailNe]

theorem validated_removeFastLookup_eq
    {l : List FastLookup} {id : Nat} {lookup : FastLookup}
    (hPair : l.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hFind : l.find? (fun x => x.id == id) = some lookup)
    (hNotValidated : lookup.stage ≠ .validated) :
    (l.filter (fun x => x.id != id)).filter
        (fun x => decide (x.stage = .validated)) =
      l.filter (fun x => decide (x.stage = .validated)) := by
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
            have hStageFalse : decide (head.stage = FastLookupStage.validated) = false := by
              simp [hNotValidated]
            have hTailNe : ∀ x ∈ tail, x.id ≠ id := by
              intro x hx hX
              apply hHead x hx
              exact hHeadId.trans hX.symm
            have hTailFilter := filter_validated_and_id_ne_eq
              (l := tail) (id := id) hTailNe
            simp [List.filter, hIdNe, hStageFalse, hTailFilter]
          · have hIdFalse : (head.id == id) = false := by
              exact Bool.not_eq_true _ |>.mp (by simpa using hId)
            have hFindTail : tail.find? (fun x => x.id == id) = some lookup := by
              simpa [List.find?, hIdFalse] using hFind
            have hTailResult := ih hTail hFindTail
            have hIdNe : (head.id != id) = true := bne_iff_ne.mpr hId
            by_cases hStage : decide (head.stage = FastLookupStage.validated) = true
            · simpa [List.filter, hIdFalse, hIdNe, hStage] using hTailResult
            · simpa [List.filter, hIdFalse, hIdNe, hStage] using hTailResult

theorem validated_removeFastLookup_filter_eq
    {l : List FastLookup} {id : Nat} :
    (l.filter (fun x => x.id != id)).filter
        (fun x => decide (x.stage = .validated)) =
      (l.filter (fun x => decide (x.stage = .validated))).filter
        (fun x => x.id != id) := by
  rw [List.filter_filter, List.filter_filter]
  simp [Bool.and_comm]

theorem updatePublicationState_eq_self_of_ne
    (p : Publication) (slot : SlotId) (gen : Generation) (st : PublicationState)
    (hNe : ¬ (p.slot = slot ∧ p.generation = gen)) :
    (if p.slot == slot && p.generation == gen then { p with state := st } else p) = p := by
  have hBool : (p.slot == slot && p.generation == gen) = false := by
    cases h1 : p.slot == slot
    · rfl
    · cases h2 : p.generation == gen
      · rfl
      · have hS := beq_iff_eq.mp h1
        have hG := beq_iff_eq.mp h2
        exact False.elim (hNe ⟨hS, hG⟩)
  rw [hBool]
  rfl

theorem updatePublicationState_eq_stale_of_eq
    (p : Publication) (slot : SlotId) (gen : Generation)
    (hEq : p.slot = slot ∧ p.generation = gen) :
    (if p.slot == slot && p.generation == gen then { p with state := .stale } else p) = { p with state := .stale } := by
  rcases hEq with ⟨hS, hG⟩
  have h1 : (p.slot == slot) = true := beq_iff_eq.mpr hS
  have h2 : (p.generation == gen) = true := beq_iff_eq.mpr hG
  simp only [h1, h2, Bool.and_self, ite_true]

theorem pairwise_updatePublicationState
    {l : List Publication} {slot : SlotId} {gen : Generation} {st : PublicationState}
    (hPair : l.Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation)) :
    (l.map (fun p => if p.slot == slot && p.generation == gen then { p with state := st } else p)).Pairwise
      (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation) := by
  apply pairwise_map hPair
  intro a _ b _ hR
  split <;> split <;> exact hR

theorem pairwise_updateClosingPublications
    {l : List Publication}
    (hPair : l.Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation)) :
    (l.map (fun p => if p.state = .live then { p with state := .closing } else p)).Pairwise
      (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation) := by
  apply pairwise_map hPair
  intro a _ b _ hR
  split <;> split <;> exact hR

theorem noLiveSlots_of_eq_slots
    {slots1 slots2 : List SlotState} (hSlots : slots1 = slots2)
    (hNL : ∀ (slot : Nat) (h : slot < slots2.length), ¬ (slots2.get ⟨slot, h⟩).IsLive) :
    ∀ (slot : Nat) (h : slot < slots1.length), ¬ (slots1.get ⟨slot, h⟩).IsLive := by
  subst hSlots
  exact hNL

theorem closeSlot_not_live (slot : SlotState) :
    ¬ (closeSlot slot).IsLive := by
  cases slot with
  | vacant g =>
      dsimp [closeSlot]
      cases nextGeneration? g <;> (intro h; cases h)
  | live g =>
      dsimp [closeSlot]
      cases nextGeneration? g <;> (intro h; cases h)
  | retired =>
      intro h
      cases h

theorem noLiveSlots_of_map_closeSlot
    {slots1 slots2 : List SlotState} (hSlots : slots1 = slots2.map closeSlot) :
    ∀ (slot : Nat) (h : slot < slots1.length), ¬ (slots1.get ⟨slot, h⟩).IsLive := by
  subst hSlots
  intro slot hB
  simp only [List.get_eq_getElem]
  have hB' : slot < slots2.length := by simpa using hB
  rw [List.getElem_map]
  exact closeSlot_not_live (slots2.get ⟨slot, hB'⟩)

theorem liveSnapshotRoot_from_sound
    {s : State}
    (hLiveSnap : s.LiveSnapshotSound)
    (hLivePub : s.LivePublicationSound) :
    s.LiveSnapshotRootIsLive := by
  intro binding hMem
  rcases hLiveSnap binding hMem with ⟨pub, hPubMem, hSlotEq, hGenEq, hLiveState⟩
  rcases hLivePub pub hPubMem hLiveState with ⟨hInB, hSlotLive⟩
  rcases binding with ⟨bslot, bgen⟩
  rcases pub with ⟨pslot, pgen, pst⟩
  dsimp at hSlotEq hGenEq hSlotLive hInB ⊢
  subst hSlotEq hGenEq
  exact ⟨hInB, hSlotLive⟩

theorem Step.invariant_preserved
    {s s' : State} {e : Event}
    (hInv : s.Invariant)
    (hStep : Step s e s') :
    s'.Invariant := by
  rcases hInv with ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub, hLiveSnap, hLiveSnapRoot, hFastSound, hLeaseAcc, hClosedNoLive⟩
  cases hStep with
  | insertFresh hReg hNoSnap hNoPub =>
      rename_i reg'
      have hRegInv : reg'.slots = s.registry.slots ++ [SlotState.live 1] ∧
                     reg'.activeLeases = s.registry.activeLeases ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session := by
        cases hReg with
        | insertFresh _ => refine ⟨rfl, rfl, rfl, rfl⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession⟩
      have hPubUniq' : (s.publications ++ [Publication.mk s.registry.slots.length 1 .live]).Pairwise
          (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation) := by
        apply pairwise_append_singleton hPubUniq
        intro y hy
        have hNot := findPublication?_none hNoPub y hy
        by_cases hS : y.slot = s.registry.slots.length
        · right
          intro hG
          exact hNot ⟨hS, hG⟩
        · left; exact hS
      have hSnapUniq' : (s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1]).Pairwise
          (fun lhs rhs => lhs.slot ≠ rhs.slot) := by
        apply pairwise_append_singleton hSnapUniq
        intro y hy
        exact findSnapshot?_none hNoSnap y hy
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk s.registry.slots.length 1 .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1] } := by
        intro pub hMem hLive
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld =>
            rcases hLivePub pub hOld hLive with ⟨hInBounds, hSlotLive⟩
            rw [hSlots]
            have hInBounds' : pub.slot < (s.registry.slots ++ [SlotState.live 1]).length := by
              rw [List.length_append]; exact Nat.lt_add_right 1 hInBounds
            refine ⟨hInBounds', ?_⟩
            simp only [List.get_eq_getElem]
            rw [List.getElem_append_left hInBounds]
            exact hSlotLive
        | inr hNew =>
            subst hNew
            dsimp
            rw [hSlots]
            have hInBounds' : s.registry.slots.length < (s.registry.slots ++ [SlotState.live 1]).length := by
              rw [List.length_append]; exact Nat.lt_add_of_pos_right (by decide)
            refine ⟨hInBounds', by simp⟩
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk s.registry.slots.length 1 .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1] } := by
        intro binding hMem
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld =>
            rcases hLiveSnap binding hOld with ⟨pub, hPubMem, hSlotEq, hGenEq, hLiveState⟩
            refine ⟨pub, List.mem_append_left _ hPubMem, hSlotEq, hGenEq, hLiveState⟩
        | inr hNew =>
            subst hNew
            refine ⟨Publication.mk s.registry.slots.length 1 .live, ?_, rfl, rfl, rfl⟩
            rw [List.mem_append]
            right
            exact List.mem_singleton_self _
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk s.registry.slots.length 1 .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1] } := by
        intro lookup hMem
        rcases hFastSound lookup hMem with ⟨hSess, pub, hPubMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, pub, List.mem_append_left _ hPubMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk s.registry.slots.length 1 .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1] } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk s.registry.slots.length 1 .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk s.registry.slots.length 1] } := by
        intro hCl
        cases hReg with
        | insertFresh hMay =>
            dsimp [Registry.State.MayInsert] at hMay
            rw [hClosed] at hCl
            rw [hCl] at hMay
            contradiction
      exact ⟨hPubUniq', hSnapUniq', hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | insertReuse hReg hNoSnap hNoPub =>
      rename_i reg' slot generation
      have hRegInv : reg'.slots = s.registry.slots.set slot (SlotState.live generation) ∧
                     reg'.activeLeases = s.registry.activeLeases ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     slot < s.registry.slots.length := by
        cases hReg with
        | insertReuse _ hInB _ => refine ⟨rfl, rfl, rfl, rfl, hInB⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hInBounds⟩
      have hPubUniq' : (s.publications ++ [Publication.mk slot generation .live]).Pairwise
          (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation) := by
        apply pairwise_append_singleton hPubUniq
        intro y hy
        have hNot := findPublication?_none hNoPub y hy
        by_cases hS : y.slot = slot
        · right
          intro hG
          exact hNot ⟨hS, hG⟩
        · left; exact hS
      have hSnapUniq' : (s.snapshot ++ [SnapshotBinding.mk slot generation]).Pairwise
          (fun lhs rhs => lhs.slot ≠ rhs.slot) := by
        apply pairwise_append_singleton hSnapUniq
        intro y hy
        exact findSnapshot?_none hNoSnap y hy
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk slot generation .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk slot generation] } := by
        intro pub hMem hLive
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld =>
            rcases hLivePub pub hOld hLive with ⟨hOldInB, hSlotLive⟩
            rw [hSlots]
            have hLen : (s.registry.slots.set slot (SlotState.live generation)).length = s.registry.slots.length := List.length_set
            have hInB' : pub.slot < (s.registry.slots.set slot (SlotState.live generation)).length := by
              rw [hLen]; exact hOldInB
            refine ⟨hInB', ?_⟩
            simp only [List.get_eq_getElem]
            by_cases hEqSlot : pub.slot = slot
            · rcases pub with ⟨pslot, pgen, pst⟩
              dsimp at hEqSlot hSlotLive ⊢
              subst hEqSlot
              cases hReg with
              | insertReuse _ _ hVacant =>
                  simp only [List.get_eq_getElem] at hVacant
                  rw [hVacant] at hSlotLive
                  contradiction
            · rw [List.getElem_set_ne (Ne.symm hEqSlot)]
              exact hSlotLive
        | inr hNew =>
            subst hNew
            dsimp
            rw [hSlots]
            have hLen : (s.registry.slots.set slot (SlotState.live generation)).length = s.registry.slots.length := List.length_set
            have hInB' : slot < (s.registry.slots.set slot (SlotState.live generation)).length := by
              rw [hLen]; exact hInBounds
            refine ⟨hInB', ?_⟩
            simp only [List.getElem_set_self]
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk slot generation .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk slot generation] } := by
        intro binding hMem
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld =>
            rcases hLiveSnap binding hOld with ⟨pub, hPubMem, hSlotEq, hGenEq, hLiveState⟩
            refine ⟨pub, List.mem_append_left _ hPubMem, hSlotEq, hGenEq, hLiveState⟩
        | inr hNew =>
            subst hNew
            refine ⟨Publication.mk slot generation .live, ?_, rfl, rfl, rfl⟩
            rw [List.mem_append]
            right
            exact List.mem_singleton_self _
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk slot generation .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk slot generation] } := by
        intro lookup hMem
        rcases hFastSound lookup hMem with ⟨hSess, pub, hPubMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, pub, List.mem_append_left _ hPubMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk slot generation .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk slot generation] } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          publications := s.publications ++ [Publication.mk slot generation .live],
          snapshot := s.snapshot ++ [SnapshotBinding.mk slot generation] } := by
        intro hCl
        cases hReg with
        | insertReuse hMay _ _ =>
            dsimp [Registry.State.MayInsert] at hMay
            rw [hClosed] at hCl
            rw [hCl] at hMay
            contradiction
      exact ⟨hPubUniq', hSnapUniq', hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | removeReuse hReg hPub hLive =>
      rename_i reg' token nextGen pub
      have hRegInv : reg'.slots = s.registry.slots.set token.slot (SlotState.vacant nextGen) ∧
                     reg'.activeLeases = s.registry.activeLeases ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     ∃ (hInB : token.slot < s.registry.slots.length),
                       s.registry.slots.get ⟨token.slot, hInB⟩ = SlotState.live token.generation := by
        cases hReg with
        | removeReuse _ hInB hL _ => refine ⟨rfl, rfl, rfl, rfl, ⟨hInB, hL⟩⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, ⟨hInB, hLiveSlot⟩⟩
      have hPubUniq' := @pairwise_updatePublicationState s.publications token.slot token.generation .stale hPubUniq
      have hSnapUniq' : (s.removeSnapshot token.slot).Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot) := by
        dsimp [State.removeSnapshot]
        exact pairwise_filter _ hSnapUniq
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro p hMem hLiveP
        dsimp [State.updatePublicationState] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hOrigEq⟩
        by_cases hMatch : orig.slot = token.slot ∧ orig.generation = token.generation
        · have hUpd := updatePublicationState_eq_stale_of_eq orig token.slot token.generation hMatch
          rw [hUpd] at hOrigEq
          subst hOrigEq
          dsimp at hLiveP
          contradiction
        · have hUpd := updatePublicationState_eq_self_of_ne orig token.slot token.generation .stale hMatch
          rw [hUpd] at hOrigEq
          subst hOrigEq
          rcases hLivePub orig hOrigMem hLiveP with ⟨hOldInB, hSlotLive⟩
          rw [hSlots]
          have hLen : (s.registry.slots.set token.slot (SlotState.vacant nextGen)).length = s.registry.slots.length := List.length_set
          have hInB' : orig.slot < (s.registry.slots.set token.slot (SlotState.vacant nextGen)).length := by
            rw [hLen]; exact hOldInB
          refine ⟨hInB', ?_⟩
          simp only [List.get_eq_getElem]
          by_cases hEqSlot : orig.slot = token.slot
          · rcases orig with ⟨oslot, ogen, ost⟩
            dsimp at hEqSlot hSlotLive hMatch ⊢
            subst hEqSlot
            have hEq : SlotState.live ogen = SlotState.live token.generation := by
              simp only [List.get_eq_getElem] at hLiveSlot
              rw [← hSlotLive, hLiveSlot]
            cases hEq
            exact False.elim (hMatch ⟨rfl, rfl⟩)
          · rw [List.getElem_set_ne (Ne.symm hEqSlot)]
            exact hSlotLive
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro binding hMem
        dsimp [State.removeSnapshot] at hMem
        have hSnapMem := mem_of_mem_filter hMem
        have hSlotNe : binding.slot ≠ token.slot := bne_iff_ne.mp (List.mem_filter.mp hMem).2
        rcases hLiveSnap binding hSnapMem with ⟨origPub, hOrigPubMem, hSlotEq, hGenEq, hOrigLive⟩
        have hOrigNotTarget : ¬ (origPub.slot = token.slot ∧ origPub.generation = token.generation) := by
          intro ⟨hS, _⟩
          rw [hSlotEq] at hS
          exact hSlotNe hS
        refine ⟨origPub, ?_, hSlotEq, hGenEq, hOrigLive⟩
        dsimp [State.updatePublicationState]
        apply List.mem_map.mpr
        refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_self_of_ne origPub token.slot token.generation .stale hOrigNotTarget⟩
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro lookup hMem
        rcases hFastSound lookup hMem with ⟨hSess, origPub, hOrigPubMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, ?_⟩
        dsimp [State.updatePublicationState]
        by_cases hMatch : origPub.slot = token.slot ∧ origPub.generation = token.generation
        · refine ⟨{ origPub with state := PublicationState.stale }, ?_, by dsimp; exact hSlotEq, by dsimp; exact hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_stale_of_eq origPub token.slot token.generation hMatch⟩
        · refine ⟨origPub, ?_, hSlotEq, hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_self_of_ne origPub token.slot token.generation .stale hMatch⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro hCl
        rw [hClosed] at hCl
        rcases hClosedNoLive hCl with ⟨hNoLiveSlots, _, _, _⟩
        have hNotLive := hNoLiveSlots token.slot hInB
        simp only [hLiveSlot, SlotState.IsLive] at hNotLive
        cases hNotLive True.intro
      exact ⟨hPubUniq', hSnapUniq', hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | removeRetire hReg hPub hLive =>
      rename_i reg' token pub
      have hRegInv : reg'.slots = s.registry.slots.set token.slot SlotState.retired ∧
                     reg'.activeLeases = s.registry.activeLeases ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     ∃ (hInB : token.slot < s.registry.slots.length),
                       s.registry.slots.get ⟨token.slot, hInB⟩ = SlotState.live token.generation := by
        cases hReg with
        | removeRetire _ hInB hL _ => refine ⟨rfl, rfl, rfl, rfl, ⟨hInB, hL⟩⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, ⟨hInB, hLiveSlot⟩⟩
      have hPubUniq' := @pairwise_updatePublicationState s.publications token.slot token.generation .stale hPubUniq
      have hSnapUniq' : (s.removeSnapshot token.slot).Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot) := by
        dsimp [State.removeSnapshot]
        exact pairwise_filter _ hSnapUniq
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro p hMem hLiveP
        dsimp [State.updatePublicationState] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hOrigEq⟩
        by_cases hMatch : orig.slot = token.slot ∧ orig.generation = token.generation
        · have hUpd := updatePublicationState_eq_stale_of_eq orig token.slot token.generation hMatch
          rw [hUpd] at hOrigEq
          subst hOrigEq
          dsimp at hLiveP
          contradiction
        · have hUpd := updatePublicationState_eq_self_of_ne orig token.slot token.generation .stale hMatch
          rw [hUpd] at hOrigEq
          subst hOrigEq
          rcases hLivePub orig hOrigMem hLiveP with ⟨hOldInB, hSlotLive⟩
          rw [hSlots]
          have hLen : (s.registry.slots.set token.slot SlotState.retired).length = s.registry.slots.length := List.length_set
          have hInB' : orig.slot < (s.registry.slots.set token.slot SlotState.retired).length := by
            rw [hLen]; exact hOldInB
          refine ⟨hInB', ?_⟩
          simp only [List.get_eq_getElem]
          by_cases hEqSlot : orig.slot = token.slot
          · rcases orig with ⟨oslot, ogen, ost⟩
            dsimp at hEqSlot hSlotLive hMatch ⊢
            subst hEqSlot
            have hEq : SlotState.live ogen = SlotState.live token.generation := by
              simp only [List.get_eq_getElem] at hLiveSlot
              rw [← hSlotLive, hLiveSlot]
            cases hEq
            exact False.elim (hMatch ⟨rfl, rfl⟩)
          · rw [List.getElem_set_ne (Ne.symm hEqSlot)]
            exact hSlotLive
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro binding hMem
        dsimp [State.removeSnapshot] at hMem
        have hSnapMem := mem_of_mem_filter hMem
        have hSlotNe : binding.slot ≠ token.slot := bne_iff_ne.mp (List.mem_filter.mp hMem).2
        rcases hLiveSnap binding hSnapMem with ⟨origPub, hOrigPubMem, hSlotEq, hGenEq, hOrigLive⟩
        have hOrigNotTarget : ¬ (origPub.slot = token.slot ∧ origPub.generation = token.generation) := by
          intro ⟨hS, _⟩
          rw [hSlotEq] at hS
          exact hSlotNe hS
        refine ⟨origPub, ?_, hSlotEq, hGenEq, hOrigLive⟩
        dsimp [State.updatePublicationState]
        apply List.mem_map.mpr
        refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_self_of_ne origPub token.slot token.generation .stale hOrigNotTarget⟩
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro lookup hMem
        rcases hFastSound lookup hMem with ⟨hSess, origPub, hOrigPubMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, ?_⟩
        dsimp [State.updatePublicationState]
        by_cases hMatch : origPub.slot = token.slot ∧ origPub.generation = token.generation
        · refine ⟨{ origPub with state := PublicationState.stale }, ?_, by dsimp; exact hSlotEq, by dsimp; exact hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_stale_of_eq origPub token.slot token.generation hMatch⟩
        · refine ⟨origPub, ?_, hSlotEq, hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, updatePublicationState_eq_self_of_ne origPub token.slot token.generation .stale hMatch⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          publications := s.updatePublicationState token.slot token.generation .stale,
          snapshot := s.removeSnapshot token.slot } := by
        intro hCl
        rw [hClosed] at hCl
        rcases hClosedNoLive hCl with ⟨hNoLiveSlots, _, _, _⟩
        have hNotLive := hNoLiveSlots token.slot hInB
        simp only [hLiveSlot, SlotState.IsLive] at hNotLive
        cases hNotLive True.intro
      exact ⟨hPubUniq', hSnapUniq', hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | beginFastObservation hNoReader hSnap hSnapGen hPub hAuth hLive =>
      rename_i readerId token pub binding
      have hFastUniq' :
          (s.fastLookups ++ [FastLookup.mk readerId token .observed]).Pairwise
            (fun lhs rhs => lhs.id ≠ rhs.id) := by
        apply pairwise_append_singleton hFastUniq
        intro y hy
        exact findFastLookup?_none hNoReader y hy
      have hLivePub' : State.LivePublicationSound { s with
          fastLookups := s.fastLookups ++ [FastLookup.mk readerId token .observed] } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with
          fastLookups := s.fastLookups ++ [FastLookup.mk readerId token .observed] } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          fastLookups := s.fastLookups ++ [FastLookup.mk readerId token .observed] } := by
        intro lookup hMem
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld => exact hFastSound lookup hOld
        | inr hNew =>
            subst hNew
            dsimp
            have hPMem := List.mem_of_find?_eq_some hPub
            have ⟨hPubSlot, hPubGen⟩ := findPublication?_some_prop hPub
            exact ⟨hAuth, pub, hPMem, hPubSlot, hPubGen⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          fastLookups := s.fastLookups ++ [FastLookup.mk readerId token .observed] } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [validated_filter_append_observed (lookup := FastLookup.mk readerId token .observed) rfl]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          fastLookups := s.fastLookups ++ [FastLookup.mk readerId token .observed] } := by
        intro hCl
        rcases hClosedNoLive hCl with ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
        exact ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | acquireTentativeLease hLookup hObs hNotSealed hNotClosed =>
      rename_i readerId lookup
      have hFastUniq' := pairwise_updateFastLookupStage
        (id := readerId) (stage := FastLookupStage.tentative) hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          fastLookups := s.updateFastLookupStage readerId .tentative } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with
          fastLookups := s.updateFastLookupStage readerId .tentative } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          fastLookups := s.updateFastLookupStage readerId .tentative } := by
        intro l hMem
        dsimp [State.updateFastLookupStage] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hMap⟩
        rcases hFastSound orig hOrigMem with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        have hToken : l.token = orig.token := by
          rw [← hMap]
          split <;> rfl
        refine ⟨by rw [hToken]; exact hSess, p, hPMem, by rw [hToken]; exact hSlotEq, by rw [hToken]; exact hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          fastLookups := s.updateFastLookupStage readerId .tentative } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.updateFastLookupStage]
        have hLen := length_validated_updateFastLookupStage_tentative_eq
          hFastUniq hLookup hObs
        rw [hLen]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          fastLookups := s.updateFastLookupStage readerId .tentative } := by
        intro hCl
        rw [hCl] at hNotClosed
        contradiction
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | abandonObservation hLookup hObs =>
      rename_i readerId lookup
      have hFastUniq' :
          (s.removeFastLookup readerId).Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := by
        dsimp [State.removeFastLookup]
        exact pairwise_filter _ hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          fastLookups := s.removeFastLookup readerId } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with
          fastLookups := s.removeFastLookup readerId } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          fastLookups := s.removeFastLookup readerId } := by
        intro l hMem
        dsimp [State.removeFastLookup] at hMem
        exact hFastSound l (mem_of_mem_filter hMem)
      have hLeaseAcc' : State.LeaseAccounting { s with
          fastLookups := s.removeFastLookup readerId } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.removeFastLookup]
        rw [validated_removeFastLookup_eq hFastUniq hLookup (by simp [hObs])]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          fastLookups := s.removeFastLookup readerId } := by
        intro hCl
        rcases hClosedNoLive hCl with ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
        exact ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | validateFastLookup hLookup hTentative hPub hLive hReg =>
      rename_i reg' readerId lookup origPub
      have hRegInv : reg'.slots = s.registry.slots ∧
                     reg'.activeLeases = s.registry.activeLeases + 1 ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     s.registry.closed = false ∧
                     lookup.token.session = s.registry.session := by
        cases hReg with
        | beginLookup hNotClosed hAuth _ _ =>
            refine ⟨rfl, rfl, rfl, rfl, hNotClosed, hAuth⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hNotClosed, hAuth⟩
      have hFastUniq' := pairwise_updateFastLookupStage
        (id := readerId) (stage := FastLookupStage.validated) hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          fastLookups := s.updateFastLookupStage readerId .validated } := by
        intro p hMem hLiveP
        rw [hSlots]
        exact hLivePub p hMem hLiveP
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          fastLookups := s.updateFastLookupStage readerId .validated } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          fastLookups := s.updateFastLookupStage readerId .validated } := by
        intro l hMem
        dsimp [State.updateFastLookupStage] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hMap⟩
        rcases hFastSound orig hOrigMem with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        have hToken : l.token = orig.token := by
          rw [← hMap]
          split <;> rfl
        refine ⟨by rw [hToken, hSession]; exact hSess, p, hPMem, by rw [hToken]; exact hSlotEq, by rw [hToken]; exact hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          fastLookups := s.updateFastLookupStage readerId .validated } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.updateFastLookupStage]
        have hLen := length_validated_updateFastLookupStage_validated
          hFastUniq hLookup hTentative
        rw [hLeases, hLen]
        exact Nat.add_le_add_right hLeaseAcc 1
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          fastLookups := s.updateFastLookupStage readerId .validated } := by
        intro hCl
        rw [hClosed] at hCl
        rw [hCl] at hNotClosed
        contradiction
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | rejectTentativeFastLookup hLookup hTentative hPub hNotLive =>
      rename_i readerId lookup origPub
      have hFastUniq' :
          (s.removeFastLookup readerId).Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := by
        dsimp [State.removeFastLookup]
        exact pairwise_filter _ hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          fastLookups := s.removeFastLookup readerId } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with
          fastLookups := s.removeFastLookup readerId } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          fastLookups := s.removeFastLookup readerId } := by
        intro l hMem
        dsimp [State.removeFastLookup] at hMem
        exact hFastSound l (mem_of_mem_filter hMem)
      have hLeaseAcc' : State.LeaseAccounting { s with
          fastLookups := s.removeFastLookup readerId } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.removeFastLookup]
        rw [validated_removeFastLookup_eq hFastUniq hLookup (by simp [hTentative])]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          fastLookups := s.removeFastLookup readerId } := by
        intro hCl
        rcases hClosedNoLive hCl with ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
        exact ⟨hNoLive, hSnapNil, hNoPubLive, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive⟩

  | completeFastLookup hLookup hValidated hReg =>
      rename_i reg' readerId lookup
      have hRegInv : reg'.slots = s.registry.slots ∧
                     reg'.activeLeases = s.registry.activeLeases - 1 ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     s.registry.activeLeases > 0 := by
        cases hReg with
        | endLookup hL => refine ⟨rfl, rfl, rfl, rfl, hL⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hL⟩
      have ⟨hLookupMem, hLookupId⟩ := findFastLookup?_mem_and_id hLookup
      have hFastUniq' : (s.removeFastLookup readerId).Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := by
        dsimp [State.removeFastLookup]
        exact pairwise_filter _ hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro p hMem hLiveP
        rw [hSlots]
        exact hLivePub p hMem hLiveP
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro l hMem
        dsimp [State.removeFastLookup] at hMem
        rcases hFastSound l (mem_of_mem_filter hMem) with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, p, hPMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.removeFastLookup]
        rw [validated_removeFastLookup_filter_eq]
        have hLookupValidated : lookup ∈ s.validatedFastLookups := by
          apply List.mem_filter.mpr
          exact ⟨hLookupMem, by simp [hValidated]⟩
        have hPairValidated := pairwise_filter
          (fun x => decide (x.stage = FastLookupStage.validated)) hFastUniq
        have hLen := length_filter_ne_of_mem hPairValidated hLookupValidated
        rw [hLookupId] at hLen
        rw [hLeases]
        dsimp [State.LeaseAccounting, State.validatedFastLookups] at hLeaseAcc
        omega
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro hCl
        rw [hClosed] at hCl
        rcases hClosedNoLive hCl with ⟨hNoLiveSlots, hSnapNil, hNoLivePubs, hSealed⟩
        have hNoLive' : ∀ (slot : Nat) (h : slot < reg'.slots.length), ¬ (reg'.slots.get ⟨slot, h⟩).IsLive :=
          noLiveSlots_of_eq_slots hSlots hNoLiveSlots
        exact ⟨hNoLive', hSnapNil, hNoLivePubs, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | fallbackFastLookup hLookup hValidated hPub hNotLive hReg =>
      rename_i reg' readerId lookup origPub
      have hRegInv : reg'.slots = s.registry.slots ∧
                     reg'.activeLeases = s.registry.activeLeases - 1 ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     s.registry.activeLeases > 0 := by
        cases hReg with
        | endLookup hL => refine ⟨rfl, rfl, rfl, rfl, hL⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hL⟩
      have ⟨hLookupMem, hLookupId⟩ := findFastLookup?_mem_and_id hLookup
      have hFastUniq' : (s.removeFastLookup readerId).Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := by
        dsimp [State.removeFastLookup]
        exact pairwise_filter _ hFastUniq
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro p hMem hLiveP
        rw [hSlots]
        exact hLivePub p hMem hLiveP
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro l hMem
        dsimp [State.removeFastLookup] at hMem
        rcases hFastSound l (mem_of_mem_filter hMem) with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, p, hPMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups, State.removeFastLookup]
        rw [validated_removeFastLookup_filter_eq]
        have hLookupValidated : lookup ∈ s.validatedFastLookups := by
          apply List.mem_filter.mpr
          exact ⟨hLookupMem, by simp [hValidated]⟩
        have hPairValidated := pairwise_filter
          (fun x => decide (x.stage = FastLookupStage.validated)) hFastUniq
        have hLen := length_filter_ne_of_mem hPairValidated hLookupValidated
        rw [hLookupId] at hLen
        rw [hLeases]
        dsimp [State.LeaseAccounting, State.validatedFastLookups] at hLeaseAcc
        omega
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          fastLookups := s.removeFastLookup readerId } := by
        intro hCl
        rw [hClosed] at hCl
        rcases hClosedNoLive hCl with ⟨hNoLiveSlots, hSnapNil, hNoLivePubs, hSealed⟩
        have hNoLive' : ∀ (slot : Nat) (h : slot < reg'.slots.length), ¬ (reg'.slots.get ⟨slot, h⟩).IsLive :=
          noLiveSlots_of_eq_slots hSlots hNoLiveSlots
        exact ⟨hNoLive', hSnapNil, hNoLivePubs, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq', hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | beginSlowLookup hNotSealed hReg =>
      rename_i reg' token
      have hRegInv : reg'.slots = s.registry.slots ∧
                     reg'.activeLeases = s.registry.activeLeases + 1 ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     s.registry.closed = false := by
        cases hReg with
        | beginLookup hNotClosed _ _ _ => refine ⟨rfl, rfl, rfl, rfl, hNotClosed⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hNotClosed⟩
      have hLivePub' : State.LivePublicationSound { s with registry := reg' } := by
        intro p hMem hLiveP
        rw [hSlots]
        exact hLivePub p hMem hLiveP
      have hLiveSnap' : State.LiveSnapshotSound { s with registry := reg' } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with registry := reg' } := by
        intro l hMem
        rcases hFastSound l hMem with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, p, hPMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with registry := reg' } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact Nat.le_trans hLeaseAcc (Nat.le_succ _)
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with registry := reg' } := by
        intro hCl
        rw [hClosed] at hCl
        rw [hCl] at hNotClosed
        contradiction
      exact ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | endSlowLookup hSlowLease hReg =>
      rename_i reg'
      have hRegInv : reg'.slots = s.registry.slots ∧
                     reg'.activeLeases = s.registry.activeLeases - 1 ∧
                     reg'.closed = s.registry.closed ∧
                     reg'.session = s.registry.session ∧
                     s.registry.activeLeases > 0 := by
        cases hReg with
        | endLookup hL => refine ⟨rfl, rfl, rfl, rfl, hL⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession, hL⟩
      have hLivePub' : State.LivePublicationSound { s with registry := reg' } := by
        intro p hMem hLiveP
        rw [hSlots]
        exact hLivePub p hMem hLiveP
      have hLiveSnap' : State.LiveSnapshotSound { s with registry := reg' } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with registry := reg' } := by
        intro l hMem
        rcases hFastSound l hMem with ⟨hSess, p, hPMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, p, hPMem, hSlotEq, hGenEq⟩
      have hLeaseAcc' : State.LeaseAccounting { s with registry := reg' } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups] at hSlowLease ⊢
        rw [hLeases]
        omega
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with registry := reg' } := by
        intro hCl
        rw [hClosed] at hCl
        rcases hClosedNoLive hCl with ⟨hNoLiveSlots, hSnapNil, hNoLivePubs, hSealed⟩
        have hNoLive' : ∀ (slot : Nat) (h : slot < reg'.slots.length), ¬ (reg'.slots.get ⟨slot, h⟩).IsLive :=
          noLiveSlots_of_eq_slots hSlots hNoLiveSlots
        exact ⟨hNoLive', hSnapNil, hNoLivePubs, hSealed⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | beginSealLeaseAdmission hOpen =>
      have hLivePub' : State.LivePublicationSound { s with leaseAdmission := .sealing } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with leaseAdmission := .sealing } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with leaseAdmission := .sealing } := hFastSound
      have hLeaseAcc' : State.LeaseAccounting { s with leaseAdmission := .sealing } := hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with leaseAdmission := .sealing } := by
        intro hCl
        rcases hClosedNoLive hCl with ⟨_, _, _, hSealed⟩
        rw [hOpen] at hSealed
        contradiction
      exact ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | finishSealLeaseAdmission hSealing =>
      have hLivePub' : State.LivePublicationSound { s with leaseAdmission := .sealed } := hLivePub
      have hLiveSnap' : State.LiveSnapshotSound { s with leaseAdmission := .sealed } := hLiveSnap
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with leaseAdmission := .sealed } := hFastSound
      have hLeaseAcc' : State.LeaseAccounting { s with leaseAdmission := .sealed } := hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with leaseAdmission := .sealed } := by
        intro hCl
        rcases hClosedNoLive hCl with ⟨hNoLive, hSnapNil, hNoPubLive, _⟩
        exact ⟨hNoLive, hSnapNil, hNoPubLive, rfl⟩
      exact ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | closeRegistry hSealed hReg =>
      rename_i reg'
      have hRegInv : reg'.slots = s.registry.slots.map closeSlot ∧
                     reg'.activeLeases = s.registry.activeLeases ∧
                     reg'.closed = true ∧
                     reg'.session = s.registry.session := by
        cases hReg with
        | closeRegistry _ => refine ⟨rfl, rfl, rfl, rfl⟩
      rcases hRegInv with ⟨hSlots, hLeases, hClosed, hSession⟩
      have hPubUniq' := pairwise_updateClosingPublications hPubUniq
      have hSnapUniq' : ([] : List SnapshotBinding).Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot) := List.Pairwise.nil
      have hLivePub' : State.LivePublicationSound { s with
          registry := reg',
          publications := s.updateClosingPublications,
          snapshot := [] } := by
        intro p hMem hLiveP
        dsimp [State.updateClosingPublications] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hOrigEq⟩
        by_cases hOrigLive : orig.state = PublicationState.live
        · have hUpd : (if orig.state = PublicationState.live then
            { orig with state := PublicationState.closing } else orig) = { orig with state := PublicationState.closing } := by
            simp [hOrigLive]
          rw [hUpd] at hOrigEq
          subst hOrigEq
          dsimp at hLiveP
          contradiction
        · have hUpd : (if orig.state = PublicationState.live then
            { orig with state := PublicationState.closing } else orig) = orig := by
            simp [hOrigLive]
          rw [hUpd] at hOrigEq
          subst hOrigEq
          exact False.elim (hOrigLive hLiveP)
      have hLiveSnap' : State.LiveSnapshotSound { s with
          registry := reg',
          publications := s.updateClosingPublications,
          snapshot := [] } := by
        intro b hMem
        contradiction
      have hLiveSnapRoot' := liveSnapshotRoot_from_sound hLiveSnap' hLivePub'
      have hFastSound' : State.FastLookupSound { s with
          registry := reg',
          publications := s.updateClosingPublications,
          snapshot := [] } := by
        intro l hMem
        rcases hFastSound l hMem with ⟨hSess, origPub, hOrigPubMem, hSlotEq, hGenEq⟩
        refine ⟨by rw [hSession]; exact hSess, ?_⟩
        dsimp [State.updateClosingPublications]
        by_cases hOrigLive : origPub.state = PublicationState.live
        · refine ⟨{ origPub with state := PublicationState.closing }, ?_, by dsimp; exact hSlotEq, by dsimp; exact hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, by simp [hOrigLive]⟩
        · refine ⟨origPub, ?_, hSlotEq, hGenEq⟩
          apply List.mem_map.mpr
          refine ⟨origPub, hOrigPubMem, by simp [hOrigLive]⟩
      have hLeaseAcc' : State.LeaseAccounting { s with
          registry := reg',
          publications := s.updateClosingPublications,
          snapshot := [] } := by
        dsimp [State.LeaseAccounting, State.validatedFastLookups]
        rw [hLeases]
        exact hLeaseAcc
      have hClosedNoLive' : State.ClosedNoLiveSlots { s with
          registry := reg',
          publications := s.updateClosingPublications,
          snapshot := [] } := by
        intro _
        have hNoLive' := noLiveSlots_of_map_closeSlot hSlots
        refine ⟨hNoLive', rfl, ?_, hSealed⟩
        intro p hMem
        dsimp [State.updateClosingPublications] at hMem
        rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, hOrigEq⟩
        by_cases hOrigLive : orig.state = PublicationState.live
        · have hUpd : (if orig.state = PublicationState.live then
            { orig with state := PublicationState.closing } else orig) = { orig with state := PublicationState.closing } := by
            simp [hOrigLive]
          rw [hUpd] at hOrigEq
          subst hOrigEq
          dsimp
          decide
        · have hUpd : (if orig.state = PublicationState.live then
            { orig with state := PublicationState.closing } else orig) = orig := by
            simp [hOrigLive]
          rw [hUpd] at hOrigEq
          subst hOrigEq
          exact hOrigLive
      exact ⟨hPubUniq', hSnapUniq', hFastUniq, hLivePub', hLiveSnap', hLiveSnapRoot', hFastSound', hLeaseAcc', hClosedNoLive'⟩

  | finishClose hNoTentative hNoValidated hReg =>
      exact ⟨hPubUniq, hSnapUniq, hFastUniq, hLivePub, hLiveSnap, hLiveSnapRoot, hFastSound, hLeaseAcc, hClosedNoLive⟩

theorem Reachable.invariant_preserved
    {s s' : State}
    (hInv : s.Invariant)
    (hReach : Reachable s s') :
    s'.Invariant := by
  induction hReach with
  | refl => exact hInv
  | tail hInit hStep ih =>
      exact Step.invariant_preserved ih hStep

end XlFnFormal.Handle.Registry.Snapshot
