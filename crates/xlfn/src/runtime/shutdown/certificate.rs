use super::owner::RemovalOwner;
use super::pipeline::QuiescenceProof;
use crate::XllError;
use crate::generation::RuntimeGeneration;
use crate::lifecycle::FinalRemovalReady;
use crate::runtime::capabilities::ShutdownDeps;

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
        deps: ShutdownDeps<'runtime, A>,
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
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalOwner<'runtime, A>)>
    where
        A: crate::Addin + 'runtime,
    {
        owner.certify_final_removal(proof, deps)
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
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<Self::Certificate<'runtime, A>, (XllError, RemovalOwner<'runtime, A>)>
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
        owner: RemovalOwner<'runtime, A>,
        generation: RuntimeGeneration,
    },
    Uncommitted {
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
        owner: RemovalOwner<'runtime, A>,
    },
}

/// Open rollback never owns a committed-generation/module-epoch certificate.
/// Any temporary module epoch is consumed while issuing this value.
pub(crate) struct OpenRollbackCertificate<'runtime, A: crate::Addin> {
    proof: QuiescenceProof,
    deps: ShutdownDeps<'runtime, A>,
    owner: RemovalOwner<'runtime, A>,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    _private: (),
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    fn certify_final_removal(
        self,
        proof: QuiescenceProof,
        deps: ShutdownDeps<'runtime, A>,
    ) -> Result<FinalRemovalCertificate<'runtime, A>, (XllError, Self)> {
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
                    deps,
                    owner: self,
                    generation,
                })
            }
            FinalRemovalReady::Uncommitted => {
                if !deps.host().is_quiescent() {
                    return Err((close_certificate_error(), self));
                }
                if self.has_module_closing() {
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
        let lifecycle = deps.lifecycle_control();
        let control = lifecycle.access();
        let ready = lifecycle.open_rollback_ready(&control, self.attempt());
        drop(control);
        if ready.is_none() || !deps.host().is_quiescent() {
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
    pub(crate) fn finish(self) -> Result<RemovalOwner<'runtime, A>, (XllError, Box<Self>)> {
        let lifecycle = self.deps.lifecycle_control();
        let attempt = self.owner.attempt();
        let result = {
            let mut control = lifecycle.access();
            lifecycle.finish_open_rollback(&mut control, attempt)
        };
        if let Err(error) = result {
            return Err((error, Box::new(self)));
        }

        let deps = self.deps;
        let OpenRollbackCertificate { proof, owner, .. } = self;
        let _quiescence = proof;
        deps.observer().finish_open_rollback();
        lifecycle.notify_all();
        #[cfg(test)]
        deps.release_test_module_lease();
        Ok(owner)
    }
}

impl<'runtime, A: crate::Addin> FinalRemovalCertificate<'runtime, A> {
    pub(crate) fn finish(
        self,
    ) -> Result<(ClosedWitness, RemovalOwner<'runtime, A>), (XllError, Box<Self>)> {
        let deps = match &self {
            Self::Committed { deps, .. } | Self::Uncommitted { deps, .. } => *deps,
        };
        let expected_generation = match &self {
            Self::Committed { generation, .. } => Some(*generation),
            Self::Uncommitted { .. } => None,
        };
        let removal_attempt = match &self {
            Self::Committed { owner, .. } | Self::Uncommitted { owner, .. } => owner.attempt(),
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
            lifecycle.finish_final_removal(&mut control, removal_attempt)
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
        if committed {
            deps.observer().finish_close();
            deps.observer().publish_committed_closed();
        } else {
            deps.observer().finish_uncommitted_final_close();
        }
        lifecycle.notify_all();
        #[cfg(test)]
        deps.release_test_module_lease();
        Ok((ClosedWitness { _private: () }, owner))
    }
}
