//! Transaction ownership for one logical open attempt.

use super::{
    active_runtime_generation, open_addin_inner, report_boundary_error, rollback_active_open,
};
use crate::addin::{Addin, BuildInfo};
use crate::diagnostics::AddinId;
use crate::host_callback::HostCallbackSession;
use crate::registration::{HostMutationJournal, RegistrationDescriptor};
use crate::runtime::{
    AddinLifecycleAccess, OpenAttemptBegun, OpenGenerationStaged, OpeningGeneration, OpeningTxn,
    Runtime,
};
use crate::{XllError, XllResult};
use std::marker::PhantomData;

/// The open protocol has started, but the generation has not been staged yet.
pub(super) struct OpenBegun;

/// The add-in state and execution layers have been staged as one value.
pub(super) struct AddinStaged;

/// Host registrations are owned by the transaction journal and may now be
/// committed together with the staged generation.
pub(super) struct HostMutated;

pub(super) enum OpenFailure<'runtime, A: Addin> {
    Begun {
        transaction: Box<OpeningTransaction<'runtime, A, OpenBegun>>,
        error: XllError,
    },
    Staged {
        transaction: Box<OpeningTransaction<'runtime, A, AddinStaged>>,
        error: XllError,
    },
}

impl<A: Addin> OpenFailure<'_, A> {
    pub(super) fn rollback(self, lifecycle: &AddinLifecycleAccess<'_, A>) -> XllError {
        match self {
            Self::Begun { transaction, error } => {
                transaction.rollback(lifecycle);
                error
            }
            Self::Staged { transaction, error } => {
                transaction.rollback(lifecycle);
                error
            }
        }
    }
}

/// Owns one logical open attempt, its host registrations, and the callback
/// session that can undo host mutations made by that attempt. The stage marker
/// makes the order of generation staging, host mutation, and publication
/// explicit without an `Option`-based active flag.
pub(super) struct OpeningTransaction<'runtime, A: Addin, Stage: OpenTransactionStage> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: OpeningTxn<'runtime, A, Stage::AttemptStage>,
    journal: HostMutationJournal,
    _stage: PhantomData<fn() -> Stage>,
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A, OpenBegun> {
    pub(super) fn begin(
        runtime: &'runtime Runtime<A>,
        removal_epoch: crate::generation::RemovalEpoch,
    ) -> XllResult<Self> {
        Ok(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: runtime.begin_open_if_epoch(removal_epoch)?,
            journal: HostMutationJournal::default(),
            _stage: PhantomData,
        })
    }

    pub(super) fn stage_generation(
        mut self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTransaction<'runtime, A, AddinStaged>, OpenFailure<'runtime, A>> {
        let result = self.attempt.stage(opening);
        match result {
            Ok(attempt) => {
                let Self {
                    runtime,
                    callbacks,
                    journal,
                    ..
                } = self;
                Ok(OpeningTransaction {
                    runtime,
                    callbacks,
                    attempt,
                    journal,
                    _stage: PhantomData,
                })
            }
            Err((error, attempt, opening)) => {
                self.attempt = *attempt;
                let opening = *opening;
                self.runtime.quarantine_opening_generation(
                    active_runtime_generation(self.runtime),
                    opening,
                    crate::runtime_components::QuarantineReason::OpenStateInvariant,
                );
                Err(OpenFailure::Begun {
                    transaction: Box::new(self),
                    error,
                })
            }
        }
    }
}

impl<'runtime, A: Addin, Stage: OpenTransactionStage> OpeningTransaction<'runtime, A, Stage> {
    pub(super) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    #[cfg(feature = "async")]
    pub(super) fn stage_events(
        &mut self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.journal.pending_events = registrations;
    }

    pub(super) fn retain_journal(&mut self, journal: HostMutationJournal) {
        self.journal.merge(journal);
    }

    pub(super) fn rollback(self, lifecycle: &AddinLifecycleAccess<'_, A>) {
        let Self {
            runtime,
            mut callbacks,
            attempt,
            journal,
            ..
        } = self;
        runtime.retain_host_mutations(journal);
        rollback_active_open(runtime, lifecycle, Some(attempt), &mut callbacks);
    }
}

pub(super) trait OpenTransactionStage: Sized {
    type AttemptStage;
}

impl OpenTransactionStage for OpenBegun {
    type AttemptStage = OpenAttemptBegun;
}

impl OpenTransactionStage for AddinStaged {
    type AttemptStage = OpenGenerationStaged;
}

impl OpenTransactionStage for HostMutated {
    type AttemptStage = OpenGenerationStaged;
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A, OpenBegun> {
    pub(super) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::Begun {
            transaction: Box::new(self),
            error,
        }
    }
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A, AddinStaged> {
    pub(super) fn failure(self, error: XllError) -> OpenFailure<'runtime, A> {
        OpenFailure::Staged {
            transaction: Box::new(self),
            error,
        }
    }
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A, AddinStaged> {
    pub(super) fn stage_registrations(
        self,
        registrations: Vec<crate::registration::RegistrationId>,
    ) -> OpeningTransaction<'runtime, A, HostMutated> {
        let Self {
            runtime,
            callbacks,
            attempt,
            mut journal,
            ..
        } = self;
        journal.pending_registrations = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
        OpeningTransaction {
            runtime,
            callbacks,
            attempt,
            journal,
            _stage: PhantomData,
        }
    }
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A, HostMutated> {
    pub(super) fn commit(self) -> XllResult<()> {
        let Self {
            attempt,
            mut journal,
            ..
        } = self;
        attempt.commit(&mut journal)
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
                    diagnostic_id: crate::error::DiagnosticId::OPEN_ROLLBACK_PENDING,
                };
                report_boundary_error("xlAutoOpen pending rollback", &error);
                super::quarantine_runtime(runtime);
                return Err(error);
            }
        }

        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        let mut transaction = OpeningTransaction::begin(runtime, removal_epoch)?;
        let transaction = match super::retry_metadata_debt(runtime, transaction.callbacks_mut()) {
            Ok(()) => transaction,
            Err(error) => {
                transaction.rollback(lifecycle);
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
        transaction.stage_registrations(registrations).commit()
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
