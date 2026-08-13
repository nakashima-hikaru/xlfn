import XlFnFormal.Handle.Topics.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

theorem mem_of_mem_filter_topics {α : Type} {p : α → Bool} {x : α} {l : List α}
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

theorem pairwise_filter_topics {α : Type} {R : α → α → Prop} (p : α → Bool)
    {l : List α} (h : l.Pairwise R) : (l.filter p).Pairwise R := by
  induction h with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      dsimp [List.filter]
      split
      · refine List.Pairwise.cons ?_ ih
        intro x hx
        exact hHead x (mem_of_mem_filter_topics hx)
      · exact ih

theorem pairwise_append_singleton_topics
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

theorem no_topic_member
    {s : State} {key : TopicKey} {topic : Topic}
    (hNoTopic : s.findTopic? key = none)
    (hMem : topic ∈ s.byKey) :
    topic.key ≠ key := by
  intro hEq
  dsimp [State.findTopic?] at hNoTopic
  have hSome : (s.byKey.find? (fun candidate => candidate.key == key)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨topic, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNoTopic] at hSome
  contradiction

theorem no_initializer_member
    {s : State} {key : TopicKey} {init : Initializer}
    (hNoInitializer : s.findInitializing? key = none)
    (hMem : init ∈ s.initializing) :
    init.key ≠ key := by
  intro hEq
  dsimp [State.findInitializing?] at hNoInitializer
  have hSome : (s.initializing.find? (fun candidate => candidate.key == key)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨init, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNoInitializer] at hSome
  contradiction

theorem initial_invariant (registry : Registry.State) :
    (initialState registry).Invariant := by
  refine ⟨List.Pairwise.nil, List.Pairwise.nil, ?_⟩
  intro topic hMem
  contradiction

theorem Step.initializingKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.InitializingKeysUnique)
    (hStep : Step s e s') :
    s'.InitializingKeysUnique := by
  cases hStep with
  | beginInitialize hNotClosed hNoTopic hNoInitializer =>
      dsimp [State.InitializingKeysUnique, State.findInitializing?] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro init hMem
      exact no_initializer_member hNoInitializer hMem
  | publish hFind hNoTopic hRoot =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.key != _) hInv
  | abortInitialize hFind =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.key != _) hInv

theorem Step.committedKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.CommittedKeysUnique)
    (hStep : Step s e s') :
    s'.CommittedKeysUnique := by
  cases hStep with
  | beginInitialize => exact hInv
  | publish hFind hNoTopic hRoot =>
      rename_i key owner rtdKey token
      dsimp [State.CommittedKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      exact no_topic_member hNoTopic hMem
  | abortInitialize => exact hInv

theorem Step.committedTopicRootsValid_preserved
    {s s' : State} {e : Event}
    (hInv : s.CommittedTopicRootsValid)
    (hStep : Step s e s') :
    s'.CommittedTopicRootsValid := by
  cases hStep with
  | beginInitialize => exact hInv
  | publish hFind hNoTopic hRoot =>
      intro topic hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld => exact hInv topic hOld
      | inr hNew =>
          subst hNew
          exact hRoot
  | abortInitialize => exact hInv

theorem Step.invariant_preserved
    {s s' : State} {e : Event}
    (hInv : s.Invariant)
    (hStep : Step s e s') :
    s'.Invariant := by
  exact ⟨
    Step.initializingKeysUnique_preserved hInv.1 hStep,
    Step.committedKeysUnique_preserved hInv.2.1 hStep,
    Step.committedTopicRootsValid_preserved hInv.2.2 hStep⟩

theorem Reachable.invariant_preserved
    {s t : State}
    (hInv : s.Invariant)
    (hReach : Reachable s t) :
    t.Invariant := by
  induction hReach with
  | refl => exact hInv
  | tail _ hStep ih => exact Step.invariant_preserved ih hStep

theorem pairwise_mem_ne_topics
    {α : Type} {R : α → α → Prop} {x y : α} {l : List α}
    (hPair : l.Pairwise R) (hX : x ∈ l) (hY : y ∈ l) (hNe : x ≠ y) :
    R x y ∨ R y x := by
  induction hPair with
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

end XlFnFormal.Handle.Topics
