import XlFnFormal.Handle.Refinement.PublishedModel

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

theorem topics_invariant_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.topics.Invariant := by
  exact hInv.1

theorem publication_identities_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.PublicationIdentitiesUnique := by
  exact hInv.2.1

theorem snapshot_keys_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.SnapshotKeysUnique := by
  exact hInv.2.2.1

theorem warm_readers_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.WarmReadersUnique := by
  exact hInv.2.2.2.1

theorem live_publication_sound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LivePublicationSound := by
  exact hInv.2.2.2.2.1

theorem live_snapshot_sound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LiveSnapshotSound := by
  exact hInv.2.2.2.2.2.1

theorem live_snapshot_root_is_live_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LiveSnapshotRootIsLive := by
  exact hInv.2.2.2.2.2.2.1

theorem warm_reader_references_known_publication_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.WarmReaderReferencesKnownPublication := by
  exact hInv.2.2.2.2.2.2.2

end XlFnFormal.Handle.Refinement
