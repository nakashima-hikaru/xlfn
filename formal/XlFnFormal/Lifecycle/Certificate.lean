import XlFnFormal.Lifecycle.Safety
import XlFnFormal.Shutdown.Invariant

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

/-! These predicates model the observations required before the Rust
    `CloseCertificate` and `OpenRollbackCertificate` can be issued.  A
    committed generation has an abstract Shutdown state; an uncommitted open
    has no such ghost state and carries only the resource-quiescence witness
    supplied by its rollback certificate. -/

def ClosePrerequisites
    (lifecycle : State)
    (shutdown : Shutdown.State) : Prop :=
  lifecycle.phase = .closing ∧
  lifecycle.openAttempt = none ∧
  lifecycle.cleanupOwner = some .finalClose ∧
  shutdown.phase = .closing .finalize ∧
  shutdown.resources.Quiescent

structure CloseCertificate
    (lifecycle : State)
    (shutdown : Shutdown.State) : Prop where
  prerequisites : ClosePrerequisites lifecycle shutdown

def OpenRollbackPrerequisites
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop :=
  (lifecycle.phase = .openRollbackPending ∨ lifecycle.phase = .closing) ∧
  lifecycle.openAttempt = none ∧
  lifecycle.cleanupOwner = some .openRollback ∧
  resources.Quiescent

structure OpenRollbackCertificate
    (lifecycle : State)
    (resources : Shutdown.Resources) : Prop where
  prerequisites : OpenRollbackPrerequisites lifecycle resources

theorem CloseCertificate.lifecycle_ready
    {lifecycle : State} {shutdown : Shutdown.State}
    (certificate : CloseCertificate lifecycle shutdown) :
    lifecycle.phase = .closing ∧
    lifecycle.openAttempt = none ∧
    lifecycle.cleanupOwner = some .finalClose := by
  exact ⟨certificate.prerequisites.1,
    certificate.prerequisites.2.1,
    certificate.prerequisites.2.2.1⟩

theorem CloseCertificate.shutdown_ready
    {lifecycle : State} {shutdown : Shutdown.State}
    (certificate : CloseCertificate lifecycle shutdown) :
    shutdown.phase = .closing .finalize ∧
    shutdown.resources.Quiescent := by
  exact ⟨certificate.prerequisites.2.2.2.1,
    certificate.prerequisites.2.2.2.2⟩

theorem CloseCertificate.can_finish
    {lifecycle : State} {shutdown : Shutdown.State}
    (certificate : CloseCertificate lifecycle shutdown) :
    ∃ lifecycle' shutdown',
      Step lifecycle .finishFinalClose lifecycle' ∧
      Shutdown.Step shutdown .finishClose shutdown' := by
  rcases certificate.lifecycle_ready with
    ⟨hPhase, hNoAttempt, hOwner⟩
  rcases certificate.shutdown_ready with
    ⟨hFinalize, hQuiescent⟩
  exact ⟨{ lifecycle with phase := .closed },
    { shutdown with phase := .closed },
    Step.finishFinalClose hPhase hNoAttempt hOwner,
    Shutdown.Step.finishClose hFinalize hQuiescent⟩

theorem OpenRollbackCertificate.lifecycle_ready
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    (lifecycle.phase = .openRollbackPending ∨ lifecycle.phase = .closing) ∧
    lifecycle.openAttempt = none ∧
    lifecycle.cleanupOwner = some .openRollback := by
  exact ⟨certificate.prerequisites.1,
    certificate.prerequisites.2.1,
    certificate.prerequisites.2.2.1⟩

theorem OpenRollbackCertificate.resources_quiescent
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    resources.Quiescent :=
  certificate.prerequisites.2.2.2

theorem OpenRollbackCertificate.can_finish
    {lifecycle : State} {resources : Shutdown.Resources}
    (certificate : OpenRollbackCertificate lifecycle resources) :
    ∃ lifecycle', Step lifecycle .finishOpenRollback lifecycle' := by
  rcases certificate.lifecycle_ready with
    ⟨hPhase, hNoAttempt, hOwner⟩
  exact ⟨{ lifecycle with phase := .closed },
    Step.finishOpenRollback hPhase hNoAttempt hOwner⟩

end XlFnFormal.Lifecycle
