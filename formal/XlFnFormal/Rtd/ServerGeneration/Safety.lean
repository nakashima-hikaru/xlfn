import XlFnFormal.Rtd.ServerGeneration.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Rtd.ServerGeneration

theorem allocate_spec
    {s s' : State} {generation : ServerGeneration}
    (hAllocate : allocate? s = some (generation, s')) :
    s.last < maxGeneration ∧
    generation = s.last + 1 ∧
    s'.last = generation := by
  unfold allocate? at hAllocate
  split at hAllocate
  · cases hAllocate
    exact ⟨by assumption, rfl, rfl⟩
  · simp at hAllocate

theorem allocate_strictly_increases
    {s s' : State} {generation : ServerGeneration}
    (hAllocate : allocate? s = some (generation, s')) :
    s.last < s'.last := by
  rcases allocate_spec hAllocate with ⟨hBound, hGeneration, hState⟩
  calc
    s.last < s.last + 1 := Nat.lt_succ_self _
    _ = generation := hGeneration.symm
    _ = s'.last := hState.symm

theorem allocated_generation_nonzero
    {s s' : State} {generation : ServerGeneration}
    (hAllocate : allocate? s = some (generation, s')) :
    generation ≠ 0 := by
  rcases allocate_spec hAllocate with ⟨hBound, hGeneration, hState⟩
  rw [hGeneration]
  exact Nat.ne_of_gt (Nat.zero_lt_succ _)

theorem allocated_generation_le_max
    {s s' : State} {generation : ServerGeneration}
    (hAllocate : allocate? s = some (generation, s')) :
    generation ≤ maxGeneration := by
  rcases allocate_spec hAllocate with ⟨hBound, hGeneration, hState⟩
  rw [hGeneration]
  exact Nat.succ_le_of_lt hBound

theorem allocated_generation_ne_previous
    {s s' : State} {generation : ServerGeneration}
    (hAllocate : allocate? s = some (generation, s')) :
    generation ≠ s.last := by
  rcases allocate_spec hAllocate with ⟨hBound, hGeneration, hState⟩
  rw [hGeneration]
  exact Nat.ne_of_gt (Nat.lt_succ_self _)

theorem exhausted_allocator_rejects
    {s : State}
    (hExhausted : s.last = maxGeneration) :
    allocate? s = none := by
  simp [allocate?, hExhausted]

theorem generation_never_reused
    {s s₁ s₂ : State}
    {generation₁ generation₂ : ServerGeneration}
    (hFirst : allocate? s = some (generation₁, s₁))
    (hSecond : allocate? s₁ = some (generation₂, s₂)) :
    generation₁ ≠ generation₂ := by
  rcases allocate_spec hFirst with ⟨hFirstBound, hFirstGeneration, hFirstState⟩
  rcases allocate_spec hSecond with ⟨hSecondBound, hSecondGeneration, hSecondState⟩
  intro hEqual
  have hImpossible : s₁.last + 1 = s₁.last := by
    calc
      s₁.last + 1 = generation₂ := hSecondGeneration.symm
      _ = generation₁ := hEqual.symm
      _ = s₁.last := hFirstState.symm
  exact (Nat.ne_of_gt (Nat.lt_succ_self s₁.last)) hImpossible

end XlFnFormal.Rtd.ServerGeneration
