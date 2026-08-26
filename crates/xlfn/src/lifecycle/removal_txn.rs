use crate::XllError;
use crate::generation::{RemovalAttemptId, RuntimeGeneration};
use crate::lifecycle::{LifecyclePhase, LifecycleRemovalState};
use crate::runtime::Runtime;
use std::sync::Arc;

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "linear proof tokens are consumed by terminal transitions"
)]
pub(crate) struct QuiescenceProof {
    pub(crate) exports: crate::ingress::ExportsDrained,
    pub(crate) module_quiescent: crate::module_runtime::ModuleQuiescent,
    pub(crate) returns: crate::shutdown::ReturnsQuiescent,
    pub(crate) rtd: crate::excel_rtd::RtdQuiescent,
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    pub(crate) generation_reclaimed: crate::shutdown::GenerationReclaimed,
}

pub(crate) struct FinalRemoval;
pub(crate) struct OpenRollback;

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

#[cfg(any(test, feature = "refinement"))]
fn composition_resources_from_quiescence_proof(
    proof: &QuiescenceProof,
) -> crate::shutdown_trace::ShutdownResources {
    // These linear tokens are the concrete proof that every resource family
    // represented by the abstract snapshot has drained. Keep this projection
    // at certificate issuance so finish events cannot observe a later ad-hoc
    // runtime snapshot.
    let _proofs = (
        &proof.exports,
        &proof.rtd,
        &proof.host_callbacks,
        &proof.async_stopped,
        &proof.subscriptions_stopped,
        &proof.handles_quiescent,
        &proof.diagnostics_stopped,
        &proof.addin_quiesced,
        &proof.generation_reclaimed,
    );
    crate::shutdown_trace::ShutdownResources::quiescent_snapshot()
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    fn validate_certificate(
        &self,
        proof: &QuiescenceProof,
        accepts_phase: impl FnOnce(LifecyclePhase) -> bool,
    ) -> Option<LifecycleRemovalState> {
        let runtime = self.runtime;
        let control = runtime.lifecycle.access();
        let lifecycle_state: LifecycleRemovalState = control.removal_state();
        let services = runtime
            .lifecycle
            .load_generation_services()
            .or_else(|| control.retiring_services().map(Arc::clone));
        let services_stopped = services.as_ref().is_none_or(|services| services.is_none());
        let handles_match_generation = lifecycle_state
            .last_committed_generation
            .is_none_or(|generation| proof.handles_quiescent.generation() == Some(generation));
        let subscriptions_match_generation = lifecycle_state
            .last_committed_generation
            .is_none_or(|generation| proof.subscriptions_stopped.generation() == Some(generation));
        let services_owned = services_stopped || lifecycle_state.has_retirement();

        let certified = accepts_phase(lifecycle_state.phase)
            && lifecycle_state.open_attempt.is_none()
            && lifecycle_state.removal_attempt == Some(self.attempt)
            // `QuiescenceProof::returns` is issued only after the return
            // admission is closed and all return obligations have drained.
            // The certificate therefore consumes the proof token instead of
            // reopening the ambient return protocol here.
            && services_stopped
            && !lifecycle_state.has_opening_generation()
            && !lifecycle_state.has_current_generation()
            && services_owned
            && runtime.host.is_quiescent();

        certified
            .then_some(lifecycle_state)
            .filter(|_| handles_match_generation && subscriptions_match_generation)
    }

    fn certify_final_removal(
        self,
        proof: QuiescenceProof,
    ) -> Result<FinalRemovalCertificate<'runtime, A>, (XllError, Self)> {
        let lifecycle_state =
            match self.validate_certificate(&proof, |phase| phase == LifecyclePhase::Closing) {
                Some(state) => state,
                None => {
                    return Err((
                        XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
                        },
                        self,
                    ));
                }
            };
        let runtime = self.runtime;
        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = composition_resources_from_quiescence_proof(&proof);

        if let Some(generation) = lifecycle_state.last_committed_generation {
            if !lifecycle_state.has_module_epoch() || !lifecycle_state.module_epoch_is_current() {
                return Err((
                    XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
                    },
                    self,
                ));
            }
            let module_epoch_matches = runtime
                .lifecycle
                .access()
                .module_epoch_id()
                .is_some_and(|epoch| epoch == proof.module_quiescent.id());
            if !module_epoch_matches {
                return Err((
                    XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
                    },
                    self,
                ));
            }
            let mut control = runtime.lifecycle.access();
            if !runtime.lifecycle.clear_certified_retirement(&mut control) {
                return Err((
                    XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
                    },
                    self,
                ));
            }
            Ok(FinalRemovalCertificate::Committed {
                proof,
                #[cfg(any(test, feature = "refinement"))]
                composition_resources,
                owner: self,
                generation,
            })
        } else {
            Ok(FinalRemovalCertificate::Uncommitted {
                proof,
                #[cfg(any(test, feature = "refinement"))]
                composition_resources,
                owner: self,
            })
        }
    }

    fn certify_open_rollback(
        self,
        proof: QuiescenceProof,
    ) -> Result<OpenRollbackCertificate<'runtime, A>, (XllError, Self)> {
        if self
            .validate_certificate(&proof, |phase| {
                matches!(
                    phase,
                    LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
                )
            })
            .is_none()
        {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_CERTIFICATE,
                },
                self,
            ));
        }

        #[cfg(any(test, feature = "refinement"))]
        let composition_resources = composition_resources_from_quiescence_proof(&proof);

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
        let runtime = self.owner.runtime;
        let mut control = runtime.lifecycle.access();
        if control.removal_attempt() != Some(self.owner.attempt)
            || control.open_attempt().is_some()
            || !matches!(
                control.phase(),
                LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
            )
        {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PHASE,
                },
                Box::new(self),
            ));
        }
        let OpenRollbackCertificate {
            proof,
            #[cfg(any(test, feature = "refinement"))]
            composition_resources,
            owner,
        } = self;
        let _module_quiescent = proof.module_quiescent;
        runtime.lifecycle.finish_closed(&mut control);
        #[cfg(any(test, feature = "refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishOpenRollback(
                composition_resources,
            ),
        );
        #[cfg(any(test, feature = "refinement"))]
        if runtime.phase() != LifecyclePhase::Closed {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoOpen rollback close postcondition",
                &XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PHASE,
                },
            );
        }
        #[cfg(any(test, feature = "refinement"))]
        runtime.mark_composition_terminal_pending();
        runtime.lifecycle.notify_all();
        #[cfg(test)]
        drop(runtime.lifecycle.test_module_lease.lock().take());
        Ok(owner)
    }
}

impl<'runtime, A: crate::Addin> FinalRemovalCertificate<'runtime, A> {
    pub(crate) fn finish(
        self,
    ) -> Result<(ClosedWitness, RemovalOwner<'runtime, A>), (XllError, Box<Self>)> {
        let runtime = match &self {
            Self::Committed { owner, .. } | Self::Uncommitted { owner, .. } => owner.runtime,
        };
        let expected_generation = match &self {
            Self::Committed { generation, .. } => Some(*generation),
            Self::Uncommitted { .. } => None,
        };
        let removal_attempt = match &self {
            Self::Committed { owner, .. } | Self::Uncommitted { owner, .. } => owner.attempt,
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
        let mut control = runtime.lifecycle.access();
        if control.removal_attempt() != Some(removal_attempt) {
            return Err((
                XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
                },
                Box::new(self),
            ));
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
                let _module_quiescent = proof.module_quiescent;
                owner
            }
        };
        runtime.lifecycle.finish_closed(&mut control);
        #[cfg(any(test, feature = "refinement"))]
        if runtime.phase() != LifecyclePhase::Closed {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoRemove close postcondition",
                &XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_WAIT,
                },
            );
        }
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
        runtime.lifecycle.notify_all();
        #[cfg(test)]
        drop(runtime.lifecycle.test_module_lease.lock().take());
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

pub(crate) struct RemovalOwner<'runtime, A: crate::Addin> {
    runtime: &'runtime Runtime<A>,
    attempt: RemovalAttemptId,
    module_closing: Option<crate::module_runtime::ModuleClosing>,
}

impl<A: crate::Addin> Drop for RemovalOwner<'_, A> {
    fn drop(&mut self) {
        // An owner may be abandoned before teardown consumes the module
        // capability. Return that capability to the runtime so a waiting
        // removal request can take it over without minting a second close
        // authority.
        if let Some(module_closing) = self.module_closing.take() {
            crate::lifecycle::LifecycleAuthority::new(self.runtime)
                .install_module_closing(module_closing);
        }
        let mut control = self.runtime.lifecycle.access();
        self.runtime
            .lifecycle
            .release_removal_owner(&mut control, self.attempt);
        if control.removal_attempt().is_some() {
            crate::lifecycle::fail_stop_invariant(
                "xlAutoRemove removal-owner release",
                &XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_WAIT,
                },
            );
        }
        self.runtime.refinement.release_cleanup_owner(self.runtime);
        self.runtime.lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    pub(crate) fn new(
        runtime: &'runtime Runtime<A>,
        attempt: RemovalAttemptId,
        module_closing: crate::module_runtime::ModuleClosing,
    ) -> Self {
        Self {
            runtime,
            attempt,
            module_closing: Some(module_closing),
        }
    }

    pub(crate) fn runtime(&self) -> &'runtime Runtime<A> {
        self.runtime
    }

    pub(crate) fn take_module_closing(&mut self) -> crate::module_runtime::ModuleClosing {
        self.module_closing
            .take()
            .expect("removal owner carries module close capability")
    }
}
