use super::owner::RemovalOwner;
use super::pipeline::QuiescenceProof;
use crate::XllError;
use crate::generation::RuntimeGeneration;
use crate::lifecycle::FinalRemovalReady;

pub(crate) struct FinalRemoval;
pub(crate) struct OpenRollback;

fn close_certificate_error() -> XllError {
    XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
    }
}

pub(crate) trait TerminalCertificateKind {
    type Certificate<'runtime, A: crate::Addin>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalOwner<'runtime, A>,
        proof: QuiescenceProof,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalOwner<'runtime, A>)>
    where
        A: crate::Addin + 'runtime;
}

impl TerminalCertificateKind for FinalRemoval {
    type Certificate<'runtime, A: crate::Addin>
        = FinalRemovalCertificate<'runtime, A>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalOwner<'runtime, A>,
        proof: QuiescenceProof,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalOwner<'runtime, A>)>
    where
        A: crate::Addin + 'runtime,
    {
        owner.certify_final_removal(proof)
    }
}

impl TerminalCertificateKind for OpenRollback {
    type Certificate<'runtime, A: crate::Addin>
        = OpenRollbackCertificate<'runtime, A>
    where
        A: 'runtime;

    fn certify<'runtime, A>(
        owner: RemovalOwner<'runtime, A>,
        proof: QuiescenceProof,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalOwner<'runtime, A>)>
    where
        A: crate::Addin + 'runtime,
    {
        owner.certify_open_rollback(proof)
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
        #[cfg(any(test, feature = "refinement"))]
        composition_resources: crate::shutdown_trace::ShutdownResources,
        owner: RemovalOwner<'runtime, A>,
        generation: RuntimeGeneration,
    },
    Uncommitted {
        proof: QuiescenceProof,
        #[cfg(any(test, feature = "refinement"))]
        composition_resources: crate::shutdown_trace::ShutdownResources,
        owner: RemovalOwner<'runtime, A>,
    },
}

/// Open rollback never owns a committed-generation/module-epoch certificate.
/// Any temporary module epoch is consumed while issuing this value.
pub(crate) struct OpenRollbackCertificate<'runtime, A: crate::Addin> {
    proof: QuiescenceProof,
    #[cfg(any(test, feature = "refinement"))]
    composition_resources: crate::shutdown_trace::ShutdownResources,
    owner: RemovalOwner<'runtime, A>,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) runtime_address: usize,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) generation: Option<RuntimeGeneration>,
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    fn certify_final_removal(
        self,
        proof: QuiescenceProof,
    ) -> Result<FinalRemovalCertificate<'runtime, A>, (XllError, Self)> {
        let runtime = self.runtime();
        let lifecycle = runtime.lifecycle_control();
        let control = lifecycle.access();
        let Some(ready) = lifecycle.final_removal_ready(&control, self.attempt()) else {
            return Err((close_certificate_error(), self));
        };
        drop(control);

        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = crate::shutdown_trace::ShutdownResources::quiescent_snapshot();

        match ready {
            FinalRemovalReady::Committed {
                generation,
                module_epoch,
            } => {
                if proof.services_generation() != Some(generation)
                    || proof.module_epoch() != module_epoch
                    || !runtime.host.is_quiescent()
                {
                    return Err((close_certificate_error(), self));
                }
                if self.has_module_closing() {
                    return Err((close_certificate_error(), self));
                }
                let mut control = lifecycle.access();
                if !lifecycle.clear_certified_retirement(&mut control) {
                    return Err((close_certificate_error(), self));
                }
                Ok(FinalRemovalCertificate::Committed {
                    proof,
                    #[cfg(any(test, feature = "refinement"))]
                    composition_resources,
                    owner: self,
                    generation,
                })
            }
            FinalRemovalReady::Uncommitted => {
                if !runtime.host.is_quiescent() {
                    return Err((close_certificate_error(), self));
                }
                if self.has_module_closing() {
                    return Err((close_certificate_error(), self));
                }
                Ok(FinalRemovalCertificate::Uncommitted {
                    proof,
                    #[cfg(any(test, feature = "refinement"))]
                    composition_resources,
                    owner: self,
                })
            }
        }
    }

    fn certify_open_rollback(
        self,
        proof: QuiescenceProof,
    ) -> Result<OpenRollbackCertificate<'runtime, A>, (XllError, Self)> {
        let runtime = self.runtime();
        let lifecycle = runtime.lifecycle_control();
        let control = lifecycle.access();
        let ready = lifecycle.open_rollback_ready(&control, self.attempt());
        drop(control);
        if ready.is_none() || !runtime.host.is_quiescent() {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
                },
                self,
            ));
        }
        if self.has_module_closing() {
            return Err((close_certificate_error(), self));
        }

        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = crate::shutdown_trace::ShutdownResources::quiescent_snapshot();

        Ok(OpenRollbackCertificate {
            proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner: self,
        })
    }

    /// Consume the affine removal owner and issue the certificate shape
    /// selected by `K`. On failure, return the owner so the caller can retain
    /// the quarantine guard.
    pub(crate) fn certify<K: TerminalCertificateKind>(
        self,
        proof: QuiescenceProof,
    ) -> Result<K::Certificate<'runtime, A>, (XllError, Self)> {
        K::certify(self, proof)
    }
}

impl<'runtime, A: crate::Addin> OpenRollbackCertificate<'runtime, A> {
    pub(crate) fn finish(self) -> Result<RemovalOwner<'runtime, A>, (XllError, Box<Self>)> {
        let runtime = self.owner.runtime();
        let lifecycle = runtime.lifecycle_control();
        let attempt = self.owner.attempt();
        let result = {
            let mut control = lifecycle.access();
            lifecycle.finish_open_rollback(&mut control, attempt)
        };
        if let Err(error) = result {
            return Err((error, Box::new(self)));
        }

        let OpenRollbackCertificate {
            proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner,
        } = self;
        let _quiescence = proof;
        #[cfg(any(test, feature = "refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishOpenRollback(
                composition_resources,
            ),
        );
        #[cfg(any(test, feature = "refinement"))]
        runtime.mark_composition_terminal_pending();
        lifecycle.notify_all();
        #[cfg(test)]
        runtime.release_test_module_lease();
        Ok(owner)
    }
}

impl<'runtime, A: crate::Addin> FinalRemovalCertificate<'runtime, A> {
    pub(crate) fn finish(
        self,
    ) -> Result<(ClosedWitness, RemovalOwner<'runtime, A>), (XllError, Box<Self>)> {
        let runtime = match &self {
            Self::Committed { owner, .. } | Self::Uncommitted { owner, .. } => owner.runtime(),
        };
        let expected_generation = match &self {
            Self::Committed { generation, .. } => Some(*generation),
            Self::Uncommitted { .. } => None,
        };
        let removal_attempt = match &self {
            Self::Committed { owner, .. } | Self::Uncommitted { owner, .. } => owner.attempt(),
        };
        if expected_generation != runtime.last_committed_generation() {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_LEASE_GATE,
                },
                Box::new(self),
            ));
        }
        #[cfg(any(test, feature = "refinement"))]
        let committed = matches!(self, Self::Committed { .. });
        #[cfg(any(test, feature = "refinement"))]
        if committed && let Err(error) = runtime.refinement_hooks().finish_close(runtime) {
            return Err((error, Box::new(self)));
        }
        let lifecycle = runtime.lifecycle_control();
        let result = {
            let mut control = lifecycle.access();
            lifecycle.finish_final_removal(&mut control, removal_attempt)
        };
        if let Err(error) = result {
            return Err((error, Box::new(self)));
        }
        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = match &self {
            Self::Committed {
                composition_resources,
                ..
            }
            | Self::Uncommitted {
                composition_resources,
                ..
            } => composition_resources.clone(),
        };
        #[cfg(any(test, feature = "refinement"))]
        let generation = match &self {
            Self::Committed { generation, .. } => Some(*generation),
            Self::Uncommitted { .. } => None,
        };
        let owner = match self {
            Self::Committed { owner, proof, .. } | Self::Uncommitted { owner, proof, .. } => {
                let _quiescence = proof;
                owner
            }
        };
        #[cfg(any(test, feature = "refinement"))]
        if committed {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::PublishCommittedClosed,
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        if !committed {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishUncommittedFinalClose(
                    composition_resources,
                ),
            );
        }
        lifecycle.notify_all();
        #[cfg(test)]
        runtime.release_test_module_lease();
        Ok((
            ClosedWitness {
                #[cfg(any(test, feature = "refinement"))]
                runtime_address: std::ptr::from_ref(runtime).addr(),
                #[cfg(any(test, feature = "refinement"))]
                generation,
            },
            owner,
        ))
    }
}
