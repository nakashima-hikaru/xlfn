//! Transaction ownership for one logical open attempt.

use crate::XllError;
use crate::addin::{Addin, BuildInfo};
use crate::boundary::{report_boundary_error, write_startup_log};
use crate::diagnostics::AddinId;
use crate::generation::OpeningGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::RegistrationDescriptor;
use crate::runtime::{AddinLifecycleAccess, Runtime};
use crate::runtime_open_txn::{
    HostOpeningState, OpenAttemptBegun, OpenGenerationStaged, OpeningTxn,
};
use crate::runtime_rollback::{active_runtime_generation, rollback_open};
use crate::runtime_transactions::{open_addin_inner, rollback_active_open};

pub(crate) enum OpenFailure<'runtime, A: Addin> {
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
    pub(crate) fn rollback(self, lifecycle: &AddinLifecycleAccess<'_, A>) -> XllError {
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
    pub(crate) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::Begun {
            transaction: Box::new(self),
            error,
        }
    }

    pub(crate) fn stage_generation(
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
                transaction
                    .runtime()
                    .runtime_orchestrator()
                    .quarantine_opening_generation(
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
    pub(crate) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
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
            let outcome = rollback_open::<A>(
                runtime,
                lifecycle,
                &mut callbacks,
                active_runtime_generation(runtime),
            );
            if !outcome.unload_safe() {
                let error = XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_PENDING,
                };
                report_boundary_error("xlAutoOpen pending rollback", &error);
                crate::runtime_recovery::quarantine_runtime(runtime);
                return Err(error);
            }
        }

        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        let mut transaction = runtime
            .runtime_orchestrator()
            .begin_open_if_epoch(removal_epoch)?
            .attach_host();
        let transaction = match crate::registration::retry_metadata_debt(
            &runtime.host,
            transaction.callbacks_mut(),
        ) {
            Ok(()) => transaction,
            Err(error) => {
                rollback_active_open(lifecycle, Some(transaction));
                return Err(error);
            }
        };
        let (transaction, registrations) = open_addin_inner::<A>(
            runtime,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction,
        )
        .map_err(|failure| failure.rollback(lifecycle))?;
        let transaction = match transaction.install_lifecycle(lifecycle) {
            Ok(transaction) => transaction,
            Err((reason, transaction)) => {
                return Err(transaction
                    .failure(crate::lifecycle::lifecycle_access_error(reason))
                    .rollback(lifecycle));
            }
        };
        transaction.stage_host_mutations(registrations).commit()
    }));

    match result {
        Ok(Ok(())) => {
            write_startup_log(addin_id, "xlAutoOpen succeeded");
            1
        }
        Ok(Err(error)) => {
            write_startup_log(addin_id, &format!("xlAutoOpen failed: {error}"));
            report_boundary_error("xlAutoOpen", &error);
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            runtime.runtime_orchestrator().quarantine();
            0
        }
    }
}
