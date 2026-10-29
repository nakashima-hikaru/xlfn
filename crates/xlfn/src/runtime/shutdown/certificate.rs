use super::owner::RemovalClaimGuard;
use super::pipeline::QuiescenceProof;
use crate::XllError;
use crate::generation::RuntimeGeneration;
use crate::lifecycle::FinalRemovalReady;
use crate::runtime::capabilities::ShutdownDeps;

/// Authorization issued only after the owner, complete proof and host checks.
/// Canonical state must be checked again while the mutation lock is held.
pub(crate) struct CertifiedRetirement<'runtime, A: crate::Addin> {
    coordinator: &'runtime crate::lifecycle::LifecycleCoordinator<A>,
    attempt: crate::generation::RemovalAttemptId,
    generation: RuntimeGeneration,
    module_epoch: crate::module_runtime::ModuleEpochId,
}

impl<A: crate::Addin> CertifiedRetirement<'_, A> {
    pub(crate) fn matches(
        &self,
        coordinator: &crate::lifecycle::LifecycleCoordinator<A>,
        attempt: crate::generation::RemovalAttemptId,
        generation: RuntimeGeneration,
        module_epoch: crate::module_runtime::ModuleEpochId,
    ) -> bool {
        std::ptr::eq(self.coordinator, coordinator)
            && self.attempt == attempt
            && self.generation == generation
            && self.module_epoch == module_epoch
            && module_epoch.is_current()
    }

    pub(crate) fn attempt(&self) -> crate::generation::RemovalAttemptId {
        self.attempt
    }
}

/// Affine permission to publish the terminal canonical state. Its proof remains
/// borrowed from the certificate until the locked transition has consumed it.
pub(crate) struct CanonicalCompletion<'proof, A: crate::Addin, K> {
    coordinator: &'proof crate::lifecycle::LifecycleCoordinator<A>,
    attempt: crate::generation::RemovalAttemptId,
    proof: &'proof QuiescenceProof,
    last_committed_generation: Option<RuntimeGeneration>,
    _kind: std::marker::PhantomData<K>,
}

impl<A: crate::Addin, K> CanonicalCompletion<'_, A, K> {
    pub(crate) fn matches(
        &self,
        coordinator: &crate::lifecycle::LifecycleCoordinator<A>,
        attempt: Option<crate::generation::RemovalAttemptId>,
        module_epoch: Option<crate::module_runtime::ModuleEpochId>,
        last_committed_generation: Option<RuntimeGeneration>,
    ) -> bool {
        std::ptr::eq(self.coordinator, coordinator)
            && attempt == Some(self.attempt)
            && module_epoch == Some(self.proof.module_epoch())
            && self.proof.module_epoch().is_current()
            && last_committed_generation == self.last_committed_generation
    }

    pub(crate) fn attempt(&self) -> crate::generation::RemovalAttemptId {
        self.attempt
    }
}

pub(crate) struct FinalRemoval;
pub(crate) struct OpenRollback;

fn close_certificate_error() -> XllError {
    XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
    }
}

pub(crate) trait TerminalCertificateKind {
    const RECORD_RECLAIM: bool;
    type Certificate<'runtime, A: crate::Addin>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalClaimGuard<'runtime, A>,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalClaimGuard<'runtime, A>)>
    where
        A: crate::Addin + 'runtime;
}

impl TerminalCertificateKind for FinalRemoval {
    const RECORD_RECLAIM: bool = true;
    type Certificate<'runtime, A: crate::Addin>
        = FinalRemovalCertificate<'runtime, A>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalClaimGuard<'runtime, A>,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalClaimGuard<'runtime, A>)>
    where
        A: crate::Addin + 'runtime,
    {
        owner.certify_final_removal(proof, deps)
    }
}

impl TerminalCertificateKind for OpenRollback {
    const RECORD_RECLAIM: bool = false;
    type Certificate<'runtime, A: crate::Addin>
        = OpenRollbackCertificate<'runtime, A>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalClaimGuard<'runtime, A>,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalClaimGuard<'runtime, A>)>
    where
        A: crate::Addin + 'runtime,
    {
        owner.certify_open_rollback(proof, deps)
    }
}

/// The final-removal certificate has the same two shapes as the formal
/// lifecycle model: a committed generation owns both its generation identity
/// and module epoch, while an uncommitted close owns neither.
#[allow(
    dead_code,
    reason = "certificate fields are linear proof ownership consumed at finish"
)]
pub(crate) enum FinalRemovalCertificate<'runtime, A: crate::Addin> {
    Committed {
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
        owner: RemovalClaimGuard<'runtime, A>,
        generation: RuntimeGeneration,
    },
    Uncommitted {
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
        owner: RemovalClaimGuard<'runtime, A>,
    },
}

/// Open rollback never owns a committed-generation/module-epoch certificate.
/// Any temporary module epoch is consumed while issuing this value.
pub(crate) struct OpenRollbackCertificate<'runtime, A: crate::Addin> {
    proof: QuiescenceProof,
    deps: ShutdownDeps<'runtime, A>,
    owner: RemovalClaimGuard<'runtime, A>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClosedKind {
    Committed,
    Uncommitted,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    kind: ClosedKind,
}

impl ClosedWitness {
    fn committed() -> Self {
        Self {
            kind: ClosedKind::Committed,
        }
    }

    fn uncommitted() -> Self {
        Self {
            kind: ClosedKind::Uncommitted,
        }
    }

    pub(crate) const fn is_committed(&self) -> bool {
        matches!(self.kind, ClosedKind::Committed)
    }
}

impl<'runtime, A: crate::Addin> RemovalClaimGuard<'runtime, A> {
    fn certify_final_removal(
        self,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<FinalRemovalCertificate<'runtime, A>, (XllError, Self)> {
        if !self.belongs_to(deps.lifecycle()) {
            return Err((close_certificate_error(), self));
        }
        let lifecycle = deps.lifecycle_control();
        let control = lifecycle.access();
        let Some(ready) = lifecycle.final_removal_ready(&control, self.attempt()) else {
            return Err((close_certificate_error(), self));
        };
        drop(control);

        match ready {
            FinalRemovalReady::Committed {
                generation,
                module_epoch,
            } => {
                if proof.services_generation() != Some(generation)
                    || proof.module_epoch() != module_epoch
                    || !deps.host().is_quiescent()
                    || !deps.residency().callbacks_retained_safely()
                {
                    return Err((close_certificate_error(), self));
                }
                let authorization = CertifiedRetirement {
                    coordinator: deps.lifecycle(),
                    attempt: self.attempt(),
                    generation,
                    module_epoch,
                };
                let mut control = lifecycle.access();
                if !lifecycle.clear_certified_retirement(&mut control, authorization) {
                    return Err((close_certificate_error(), self));
                }
                Ok(FinalRemovalCertificate::Committed {
                    proof,
                    deps,
                    owner: self,
                    generation,
                })
            }
            FinalRemovalReady::Uncommitted => {
                if !deps.host().is_quiescent() || !deps.residency().callbacks_retained_safely() {
                    return Err((close_certificate_error(), self));
                }
                Ok(FinalRemovalCertificate::Uncommitted {
                    proof,
                    deps,
                    owner: self,
                })
            }
        }
    }

    fn certify_open_rollback(
        self,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<OpenRollbackCertificate<'runtime, A>, (XllError, Self)> {
        if !self.belongs_to(deps.lifecycle()) {
            return Err((close_certificate_error(), self));
        }
        let lifecycle = deps.lifecycle_control();
        let control = lifecycle.access();
        let ready = lifecycle.open_rollback_ready(&control, self.attempt());
        drop(control);
        if ready.is_none()
            || !deps.host().is_quiescent()
            || !deps.residency().callbacks_retained_safely()
        {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
                },
                self,
            ));
        }

        Ok(OpenRollbackCertificate {
            proof,
            deps,
            owner: self,
        })
    }

    /// Consume the affine removal owner and issue the certificate shape
    /// selected by `K`. On failure, return the owner so the caller can retain
    /// the quarantine guard.
    pub(crate) fn certify<K: TerminalCertificateKind>(
        self,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<K::Certificate<'runtime, A>, (XllError, Self)> {
        K::certify(self, proof, deps)
    }
}

impl<'runtime, A: crate::Addin> OpenRollbackCertificate<'runtime, A> {
    pub(crate) fn finish(self) -> Result<RemovalClaimGuard<'runtime, A>, (XllError, Box<Self>)> {
        let lifecycle = self.deps.lifecycle_control();
        if !self.owner.belongs_to(self.deps.lifecycle()) {
            return Err((close_certificate_error(), Box::new(self)));
        }
        let authorization = CanonicalCompletion::<A, OpenRollback> {
            coordinator: self.deps.lifecycle(),
            attempt: self.owner.attempt(),
            proof: &self.proof,
            last_committed_generation: self.deps.last_committed_generation(),
            _kind: std::marker::PhantomData,
        };
        let result = {
            let mut control = lifecycle.access();
            lifecycle
                .finish_open_rollback(&mut control, authorization)
                .inspect(|()| {
                    #[cfg(any(test, feature = "refinement"))]
                    self.deps.observer().finish_open_rollback(
                        &control,
                        self.deps.terminal_resources(&control, &self.proof),
                    );
                })
        };
        if let Err(error) = result {
            return Err((error, Box::new(self)));
        }

        #[cfg(test)]
        let deps = self.deps;
        let OpenRollbackCertificate { proof, owner, .. } = self;
        let _quiescence = proof;
        lifecycle.notify_all();
        #[cfg(test)]
        deps.release_test_module_lease();
        Ok(owner)
    }
}

impl<'runtime, A: crate::Addin> FinalRemovalCertificate<'runtime, A> {
    pub(crate) fn finish(
        self,
    ) -> Result<(ClosedWitness, RemovalClaimGuard<'runtime, A>), (XllError, Box<Self>)> {
        let deps = match &self {
            Self::Committed { deps, .. } | Self::Uncommitted { deps, .. } => *deps,
        };
        let expected_generation = match &self {
            Self::Committed { generation, .. } => Some(*generation),
            Self::Uncommitted { .. } => None,
        };
        let (owner, proof) = match &self {
            Self::Committed { owner, proof, .. } | Self::Uncommitted { owner, proof, .. } => {
                (owner, proof)
            }
        };
        if !owner.belongs_to(deps.lifecycle())
            || (expected_generation.is_some() && proof.services_generation() != expected_generation)
        {
            return Err((close_certificate_error(), Box::new(self)));
        }
        let authorization = CanonicalCompletion::<A, FinalRemoval> {
            coordinator: deps.lifecycle(),
            attempt: owner.attempt(),
            proof,
            last_committed_generation: expected_generation,
            _kind: std::marker::PhantomData,
        };
        if expected_generation != deps.last_committed_generation() {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_LEASE_GATE,
                },
                Box::new(self),
            ));
        }
        let committed = matches!(&self, Self::Committed { .. });
        let lifecycle = deps.lifecycle_control();
        let result = {
            let mut control = lifecycle.access();
            lifecycle
                .finish_final_removal(&mut control, authorization)
                .inspect(|()| {
                    if committed {
                        deps.observer().finish_close(&control);
                        deps.observer().publish_committed_closed(&control);
                    } else {
                        #[cfg(any(test, feature = "refinement"))]
                        {
                            let proof = match &self {
                                Self::Committed { proof, .. } | Self::Uncommitted { proof, .. } => {
                                    proof
                                }
                            };
                            deps.observer().finish_uncommitted_final_close(
                                &control,
                                deps.terminal_resources(&control, proof),
                            );
                        }
                    }
                })
        };
        if let Err(error) = result {
            return Err((error, Box::new(self)));
        }
        let owner = match self {
            Self::Committed { owner, proof, .. } | Self::Uncommitted { owner, proof, .. } => {
                let _quiescence = proof;
                owner
            }
        };
        let witness = if committed {
            ClosedWitness::committed()
        } else {
            ClosedWitness::uncommitted()
        };
        lifecycle.notify_all();
        #[cfg(test)]
        deps.release_test_module_lease();
        Ok((witness, owner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::RemovalAttemptId;
    use crate::module_runtime::ModuleEpochId;
    use crate::runtime::Runtime;
    use crate::runtime::shutdown::RemovalOwner;

    fn retired(
        runtime: &Runtime<()>,
    ) -> (RemovalClaimGuard<'_, ()>, RuntimeGeneration, ModuleEpochId) {
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let generation = runtime.protocol_generation().unwrap();
        let owner = RemovalOwner::acquire_final(runtime.shutdown_deps()).unwrap();
        runtime.wait_for_returns();
        let subscriptions = runtime.close_subscriptions().unwrap();
        runtime.shutdown_handle_topics().unwrap();
        let sealed = runtime.seal_generation_services(subscriptions).unwrap();
        runtime.finish_generation_services(sealed).unwrap();
        assert!(runtime.take_current_generation().is_some());
        let (owner, closing) = owner.into_test_parts();
        let drained = closing.seal_and_drain().unwrap();
        let (module, _exports) = drained.certify_for_test();
        runtime.disable_trace_for_test();
        (owner, generation, module.id())
    }

    fn retirement<'a>(
        runtime: &'a Runtime<()>,
        owner: &RemovalClaimGuard<'_, ()>,
        generation: RuntimeGeneration,
        module_epoch: ModuleEpochId,
    ) -> CertifiedRetirement<'a, ()> {
        CertifiedRetirement {
            coordinator: &runtime.lifecycle,
            attempt: owner.attempt(),
            generation,
            module_epoch,
        }
    }

    fn completion<'a, K>(
        runtime: &'a Runtime<()>,
        owner: &RemovalClaimGuard<'_, ()>,
        proof: &'a QuiescenceProof,
        generation: RuntimeGeneration,
    ) -> CanonicalCompletion<'a, (), K> {
        CanonicalCompletion {
            coordinator: &runtime.lifecycle,
            attempt: owner.attempt(),
            proof,
            last_committed_generation: Some(generation),
            _kind: std::marker::PhantomData,
        }
    }

    #[test]
    fn canonical_retirement_and_completion_reject_substituted_and_replayed_authority() {
        let runtime = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let (owner, generation, epoch) = retired(&runtime);
        let lifecycle = runtime.lifecycle_control();
        let proof = QuiescenceProof::for_test(Some(generation), epoch);
        let next_attempt = RemovalAttemptId::new(u64::MAX).unwrap();
        let next_generation = RuntimeGeneration::new(generation.get() + 1).unwrap();
        for mismatch in 0..3 {
            let mut authorization = retirement(&runtime, &owner, generation, epoch);
            match mismatch {
                0 => authorization.coordinator = &other.lifecycle,
                1 => authorization.attempt = next_attempt,
                2 => authorization.generation = next_generation,
                _ => unreachable!(),
            }
            let mut control = lifecycle.access();
            assert!(!lifecycle.clear_certified_retirement(&mut control, authorization));
            assert!(matches!(
                lifecycle.final_removal_ready(&control, owner.attempt()),
                Some(FinalRemovalReady::Committed { .. })
            ));
        }
        {
            let mut wrong_guard = other.lifecycle.access();
            assert!(!lifecycle.clear_certified_retirement(
                &mut wrong_guard,
                retirement(&runtime, &owner, generation, epoch)
            ));
            assert!(
                lifecycle
                    .finish_final_removal(
                        &mut wrong_guard,
                        completion::<FinalRemoval>(&runtime, &owner, &proof, generation)
                    )
                    .is_err()
            );
            assert_eq!(
                wrong_guard.phase(),
                crate::lifecycle::LifecyclePhase::Closed
            );
        }
        {
            let mut control = lifecycle.access();
            // A proof alone cannot skip canonical retirement disposal.
            assert!(
                lifecycle
                    .finish_final_removal(
                        &mut control,
                        completion::<FinalRemoval>(&runtime, &owner, &proof, generation)
                    )
                    .is_err()
            );
            assert!(lifecycle.clear_certified_retirement(
                &mut control,
                retirement(&runtime, &owner, generation, epoch)
            ));
            assert!(!lifecycle.clear_certified_retirement(
                &mut control,
                retirement(&runtime, &owner, generation, epoch)
            ));
        }
        for mismatch in 0..3 {
            let mut authorization =
                completion::<FinalRemoval>(&runtime, &owner, &proof, generation);
            match mismatch {
                0 => authorization.coordinator = &other.lifecycle,
                1 => authorization.attempt = next_attempt,
                2 => authorization.last_committed_generation = Some(next_generation),
                _ => unreachable!(),
            }
            let mut control = lifecycle.access();
            assert!(
                lifecycle
                    .finish_final_removal(&mut control, authorization)
                    .is_err()
            );
            assert_eq!(control.removal_attempt(), Some(owner.attempt()));
        }
        {
            let mut control = lifecycle.access();
            assert!(
                lifecycle
                    .finish_final_removal(
                        &mut control,
                        completion::<FinalRemoval>(&runtime, &owner, &proof, generation)
                    )
                    .is_ok()
            );
            assert!(
                lifecycle
                    .finish_final_removal(
                        &mut control,
                        completion::<FinalRemoval>(&runtime, &owner, &proof, generation)
                    )
                    .is_err()
            );
        }
        drop(owner);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }

    #[test]
    fn terminal_authority_rejects_stale_or_substituted_module_epoch() {
        let old = Runtime::<()>::new();
        let (old_owner, old_generation, old_epoch) = retired(&old);
        old.release_test_module_lease();
        let current = Runtime::<()>::new();
        let (current_owner, current_generation, current_epoch) = retired(&current);
        assert_ne!(old_epoch, current_epoch);
        let lifecycle = current.lifecycle_control();
        let mut control = lifecycle.access();
        assert!(!lifecycle.clear_certified_retirement(
            &mut control,
            retirement(&current, &current_owner, current_generation, old_epoch)
        ));
        assert!(lifecycle.clear_certified_retirement(
            &mut control,
            retirement(&current, &current_owner, current_generation, current_epoch)
        ));
        let wrong_proof = QuiescenceProof::for_test(Some(current_generation), old_epoch);
        assert!(
            lifecycle
                .finish_final_removal(
                    &mut control,
                    completion::<FinalRemoval>(
                        &current,
                        &current_owner,
                        &wrong_proof,
                        current_generation
                    )
                )
                .is_err()
        );
        drop(control);
        let lifecycle = old.lifecycle_control();
        let mut control = lifecycle.access();
        assert!(!lifecycle.clear_certified_retirement(
            &mut control,
            retirement(&old, &old_owner, old_generation, old_epoch)
        ));
        drop(control);
        drop(current_owner);
        drop(old_owner);
    }

    #[test]
    fn terminal_certificate_rejects_foreign_dependencies_and_returns_its_owner() {
        let runtime = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let (owner, generation, epoch) = retired(&runtime);
        let proof = QuiescenceProof::for_test(Some(generation), epoch);
        let Err((_, owner)) = owner.certify::<FinalRemoval>(proof, other.shutdown_deps()) else {
            panic!("foreign final-removal dependencies accepted");
        };
        let proof = QuiescenceProof::for_test(Some(generation), epoch);
        let Err((_, owner)) = owner.certify::<OpenRollback>(proof, other.shutdown_deps()) else {
            panic!("foreign rollback dependencies accepted");
        };
        assert!(owner.belongs_to(&runtime.lifecycle));
        assert!(other.lifecycle.access().removal_attempt().is_none());
        let proof = QuiescenceProof::for_test(Some(generation), epoch);
        let certificate = owner
            .certify::<FinalRemoval>(proof, runtime.shutdown_deps())
            .unwrap_or_else(|_| panic!("returned owner lost authority"));
        let (_, owner) = certificate
            .finish()
            .unwrap_or_else(|_| panic!("valid final completion"));
        drop(owner);
        assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closed);
        assert_eq!(other.phase(), crate::lifecycle::LifecyclePhase::Closed);
    }
}
