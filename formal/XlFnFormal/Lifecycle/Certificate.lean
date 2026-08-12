import XlFnFormal.Lifecycle.Safety
import XlFnFormal.Shutdown.Invariant

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

/-! The concrete runtime has one Rust `CloseCertificate` type, but the
    abstract model distinguishes the semantic paths which issue it.  A
    committed close owns a live Shutdown session; an uncommitted final close
    has no Shutdown generation at all; open rollback is a separate owner. -/

def CleanupReady
    (lifecycle : State)
    (owner : CleanupOwner)
    (resources : Shutdown.Resources) : Prop :=
  lifecycle.openAttempt = none ∧
  lifecycle.cleanupOwner = some owner ∧
  resources.Quiescent

def CommittedClosePrerequisites
    (lifecycle : State)
    (generation : AttemptId)
    (shutdown : Shutdown.State) : Prop :=
  lifecycle.phase = .closing ∧
  lifecycle.generation = generation ∧
  CleanupReady lifecycle .finalClose shutdown.resources ∧
  shutdown.phase = .closing .finalize

structure CommittedCloseCertificate
    (lifecycle : State)
    (generation : AttemptId)
    (shutdown : Shutdown.State) : Prop where
  prerequisites : CommittedClosePrerequisites lifecycle generation shutdown

def UncommittedClosePrerequisites
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop :=
  lifecycle.phase = .closing ∧
  CleanupReady lifecycle .finalClose resources

structure UncommittedCloseCertificate
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop where
  prerequisites : UncommittedClosePrerequisites lifecycle resources

def OpenRollbackPrerequisites
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop :=
  (lifecycle.phase = .openRollbackPending ∨ lifecycle.phase = .closing) ∧
  CleanupReady lifecycle .openRollback resources

structure OpenRollbackCertificate
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop where
  prerequisites : OpenRollbackPrerequisites lifecycle resources

theorem CommittedCloseCertificate.lifecycle_ready
    {lifecycle : State} {generation : AttemptId} {shutdown : Shutdown.State}
    (certificate : CommittedCloseCertificate lifecycle generation shutdown) :
    lifecycle.phase = .closing ∧
    lifecycle.generation = generation ∧
    lifecycle.openAttempt = none ∧
    lifecycle.cleanupOwner = some .finalClose := by
  rcases certificate.prerequisites with
    ⟨hPhase, hGeneration, hReady, _⟩
  exact ⟨hPhase, hGeneration, hReady.1, hReady.2.1⟩

theorem CommittedCloseCertificate.shutdown_ready
    {lifecycle : State} {generation : AttemptId} {shutdown : Shutdown.State}
    (certificate : CommittedCloseCertificate lifecycle generation shutdown) :
    shutdown.phase = .closing .finalize ∧
    shutdown.resources.Quiescent := by
  rcases certificate.prerequisites with
    ⟨_, _, hReady, hFinalize⟩
  exact ⟨hFinalize, hReady.2.2⟩

theorem CommittedCloseCertificate.can_finish_shutdown
    {lifecycle : State} {generation : AttemptId} {shutdown : Shutdown.State}
    (certificate : CommittedCloseCertificate lifecycle generation shutdown) :
    ∃ shutdown', Shutdown.Step shutdown .finishClose shutdown' := by
  rcases certificate.shutdown_ready with ⟨hFinalize, hQuiescent⟩
  exact ⟨{ shutdown with phase := .closed },
    Shutdown.Step.finishClose hFinalize hQuiescent⟩

theorem CommittedCloseCertificate.can_publish_closed
    {lifecycle : State} {generation : AttemptId} {shutdown : Shutdown.State}
    (certificate : CommittedCloseCertificate lifecycle generation shutdown) :
    ∃ lifecycle', Lifecycle.Step lifecycle .finishFinalClose lifecycle' := by
  rcases certificate.lifecycle_ready with
    ⟨hPhase, _, hNoAttempt, hOwner⟩
  exact ⟨{ lifecycle with phase := .closed },
    Lifecycle.Step.finishFinalClose hPhase hNoAttempt hOwner⟩

theorem UncommittedCloseCertificate.lifecycle_ready
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : UncommittedCloseCertificate lifecycle resources) :
    lifecycle.phase = .closing ∧
    lifecycle.openAttempt = none ∧
    lifecycle.cleanupOwner = some .finalClose := by
  rcases certificate.prerequisites with ⟨hPhase, hReady⟩
  exact ⟨hPhase, hReady.1, hReady.2.1⟩

theorem UncommittedCloseCertificate.resources_quiescent
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : UncommittedCloseCertificate lifecycle resources) :
    resources.Quiescent := by
  exact certificate.prerequisites.2.2.2

theorem UncommittedCloseCertificate.can_finish
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : UncommittedCloseCertificate lifecycle resources) :
    ∃ lifecycle', Lifecycle.Step lifecycle .finishFinalClose lifecycle' := by
  rcases certificate.lifecycle_ready with ⟨hPhase, hNoAttempt, hOwner⟩
  exact ⟨{ lifecycle with phase := .closed },
    Lifecycle.Step.finishFinalClose hPhase hNoAttempt hOwner⟩

theorem OpenRollbackCertificate.lifecycle_ready
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    (lifecycle.phase = .openRollbackPending ∨ lifecycle.phase = .closing) ∧
    lifecycle.openAttempt = none ∧
    lifecycle.cleanupOwner = some .openRollback := by
  rcases certificate.prerequisites with ⟨hPhase, hReady⟩
  exact ⟨hPhase, hReady.1, hReady.2.1⟩

theorem OpenRollbackCertificate.resources_quiescent
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    resources.Quiescent := by
  exact certificate.prerequisites.2.2.2

theorem OpenRollbackCertificate.can_finish
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    ∃ lifecycle', Lifecycle.Step lifecycle .finishOpenRollback lifecycle' := by
  rcases certificate.lifecycle_ready with ⟨hPhase, hNoAttempt, hOwner⟩
  exact ⟨{ lifecycle with phase := .closed },
    Lifecycle.Step.finishOpenRollback hPhase hNoAttempt hOwner⟩

end XlFnFormal.Lifecycle
