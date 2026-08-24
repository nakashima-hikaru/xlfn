//! Transaction ownership for one logical open attempt.

use super::{
    active_runtime_generation, open_addin_inner, report_boundary_error, rollback_active_open,
};
use crate::XllError;
use crate::addin::{Addin, BuildInfo};
use crate::diagnostics::AddinId;
use crate::host_callback::HostCallbackSession;
use crate::registration::RegistrationDescriptor;
use crate::runtime::{
    AddinLifecycleAccess, HostOpeningState, OpenAttemptBegun, OpenGenerationStaged,
    OpeningGeneration, OpeningTxn, Runtime,
};

pub(super) enum OpenFailure<'runtime, A: Addin> {
    Begun {
        transaction: Box<OpeningTxn<'runtime, A, OpenAttemptBegun, HostOpeningState>>,
        error: XllError,
    },
    Staged {
        transaction: Box<OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState>>,
        error: XllError,
    },
}

impl<A: Addin> OpenFailure<'_, A> {
    pub(super) fn rollback(self, lifecycle: &AddinLifecycleAccess<'_, A>) -> XllError {
        match self {
            Self::Begun { transaction, error } => {
                rollback_active_open(lifecycle, Some(*transaction));
                error
            }
            Self::Staged { transaction, error } => {
                rollback_active_open(lifecycle, Some(*transaction));
                error
            }
        }
    }
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, OpenAttemptBegun, HostOpeningState> {
    pub(super) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::Begun {
            transaction: Box::new(self),
            error,
        }
    }

    pub(super) fn stage_generation(
        self,
        opening: OpeningGeneration<A>,
    ) -> Result<
        OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState>,
        OpenFailure<'runtime, A>,
    > {
        match self.stage(opening) {
            Ok(transaction) => Ok(transaction),
            Err((error, transaction, opening)) => {
                let transaction = *transaction;
                transaction.runtime().quarantine_opening_generation(
                    active_runtime_generation(transaction.runtime()),
                    *opening,
                    crate::runtime_components::QuarantineReason::OpenStateInvariant,
                );
                Err(OpenFailure::Begun {
                    transaction: Box::new(transaction),
                    error,
                })
            }
        }
    }
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState> {
    pub(super) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::Staged {
            transaction: Box::new(self),
            error,
        }
    }
}

pub(crate) fn open_addin_boundary<A>(
    runtime: &Runtime<A>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    std::hint::black_box(crate::crt::effective_crt_policy());
    let removal_epoch = runtime.removal_epoch();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if runtime.phase() == crate::lifecycle::LifecyclePhase::OpenRollbackPending {
            let mut callbacks = HostCallbackSession::new();
            let outcome = super::rollback_open::<A>(
                runtime,
                lifecycle,
                &mut callbacks,
                super::active_runtime_generation(runtime),
            );
            if !outcome.unload_safe() {
                let error = XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PENDING,
                };
                report_boundary_error("xlAutoOpen pending rollback", &error);
                super::quarantine_runtime(runtime);
                return Err(error);
            }
        }

        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        let mut transaction = runtime.begin_open_if_epoch(removal_epoch)?.attach_host();
        let transaction = match super::retry_metadata_debt(runtime, transaction.callbacks_mut()) {
            Ok(()) => transaction,
            Err(error) => {
                rollback_active_open(lifecycle, Some(transaction));
                return Err(error);
            }
        };
        let (transaction, registrations) = open_addin_inner::<A>(
            runtime,
            lifecycle,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction,
        )
        .map_err(|failure| failure.rollback(lifecycle))?;
        transaction.stage_host_mutations(registrations).commit()
    }));

    match result {
        Ok(Ok(())) => {
            super::write_startup_log(addin_id, "xlAutoOpen succeeded");
            1
        }
        Ok(Err(error)) => {
            super::write_startup_log(addin_id, &format!("xlAutoOpen failed: {error}"));
            report_boundary_error("xlAutoOpen", &error);
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            super::write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            runtime.quarantine();
            0
        }
    }
}
