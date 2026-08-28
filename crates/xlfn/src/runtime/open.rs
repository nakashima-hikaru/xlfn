//! Transaction ownership for one logical open attempt.

use crate::XllError;
use crate::addin::{Addin, BuildInfo};
use crate::boundary::{report_boundary_error, write_startup_log};
use crate::diagnostics::AddinId;
use crate::generation::OpeningGeneration;
use crate::host_callback::HostCallbackSession;
use crate::registration::RegistrationDescriptor;
use crate::runtime::open_txn::{GenerationStaged, HostAttached, Initialized, OpeningTxn};
use crate::runtime::rollback::{active_runtime_generation, rollback_open};
use crate::runtime::transactions::{open_addin_inner, rollback_active_open};
use crate::runtime::{AddinLifecycleAccess, Runtime};

pub(crate) enum OpenFailure<'runtime, A: Addin> {
    HostAttached {
        transaction: Box<OpeningTxn<'runtime, A, HostAttached>>,
        error: XllError,
    },
    Initialized {
        transaction: Box<OpeningTxn<'runtime, A, Initialized<A>>>,
        error: XllError,
    },
    GenerationStaged {
        transaction: Box<OpeningTxn<'runtime, A, GenerationStaged<A>>>,
        error: XllError,
    },
}

impl<'runtime, A: Addin> OpenFailure<'runtime, A> {
    pub(crate) fn rollback(
        self,
        runtime: &'runtime Runtime<A>,
        lifecycle: &AddinLifecycleAccess<'_, A>,
    ) -> XllError {
        match self {
            Self::HostAttached { transaction, error } => {
                rollback_active_open(runtime, lifecycle, Some(*transaction));
                error
            }
            Self::Initialized { transaction, error } => {
                rollback_active_open(runtime, lifecycle, Some(*transaction));
                error
            }
            Self::GenerationStaged { transaction, error } => {
                rollback_active_open(runtime, lifecycle, Some(*transaction));
                error
            }
        }
    }
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, HostAttached> {
    pub(crate) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::HostAttached {
            transaction: Box::new(self),
            error,
        }
    }
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, Initialized<A>> {
    pub(crate) fn stage_generation(
        self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTxn<'runtime, A, GenerationStaged<A>>, OpenFailure<'runtime, A>> {
        match self.stage_opening_generation(opening) {
            Ok(transaction) => Ok(transaction),
            Err((error, transaction, opening)) => {
                let transaction = *transaction;
                transaction.deps().quarantine_opening_generation(
                    transaction.deps().protocol_generation(),
                    *opening,
                    crate::runtime_components::QuarantineReason::OpenStateInvariant,
                );
                Err(OpenFailure::Initialized {
                    transaction: Box::new(transaction),
                    error,
                })
            }
        }
    }
}

impl<'runtime, A: Addin> OpeningTxn<'runtime, A, GenerationStaged<A>> {
    pub(crate) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::GenerationStaged {
            transaction: Box::new(self),
            error,
        }
    }
}

pub(super) fn open_addin_boundary<A>(
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
                crate::runtime::recovery::quarantine_runtime(runtime);
                return Err(error);
            }
        }

        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        let mut transaction = runtime.begin_open_if_epoch(removal_epoch)?.attach_host();
        let transaction = match crate::registration::retry_metadata_debt(
            &runtime.host,
            transaction.callbacks_mut(),
        ) {
            Ok(()) => transaction,
            Err(error) => {
                rollback_active_open(runtime, lifecycle, Some(transaction));
                return Err(error);
            }
        };
        let (transaction, registrations) = open_addin_inner::<A>(
            runtime,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction,
        )
        .map_err(|failure| failure.rollback(runtime, lifecycle))?;
        let transaction = match transaction.install_lifecycle(lifecycle) {
            Ok(transaction) => transaction,
            Err((reason, transaction)) => {
                return Err(transaction
                    .failure(crate::lifecycle::lifecycle_access_error(reason))
                    .rollback(runtime, lifecycle));
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
            runtime.lifecycle_orchestrator().quarantine();
            0
        }
    }
}
