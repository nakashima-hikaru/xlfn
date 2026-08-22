//! Transaction ownership for one logical open attempt.

use super::{open_addin_inner, report_boundary_error, rollback_active_open};
use crate::addin::{Addin, BuildInfo};
use crate::diagnostics::AddinId;
use crate::host_callback::HostCallbackSession;
use crate::registration::RegistrationDescriptor;
use crate::runtime::{LifecycleThreadAccess, OpenAttemptGuard, Runtime};
use crate::{XllError, XllResult};

/// Owns one logical open attempt, its host registrations, and the callback
/// session that can undo host mutations made by that attempt. The lifecycle
/// slot holds the staged generation under this attempt until [`Self::commit`]
/// publishes it. The caller must explicitly call [`Self::commit`] or
/// [`Self::rollback`]; dropping an active transaction only quarantines the
/// runtime and never performs implicit callback cleanup.
pub(super) struct OpeningTransaction<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: Option<OpenAttemptGuard<'runtime, A>>,
    registrations: Vec<crate::registration::RegistrationId>,
}

impl<'runtime, A: Addin> OpeningTransaction<'runtime, A> {
    pub(super) fn begin(
        runtime: &'runtime Runtime<A>,
        removal_epoch: crate::generation::RemovalEpoch,
    ) -> XllResult<Self> {
        Ok(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: Some(runtime.begin_open_if_epoch(removal_epoch)?),
            registrations: Vec::new(),
        })
    }

    pub(super) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    pub(super) fn stage_registrations(
        &mut self,
        registrations: Vec<crate::registration::RegistrationId>,
    ) {
        self.registrations = registrations;
    }

    pub(super) fn commit(&mut self) -> XllResult<()> {
        self.runtime.finish_open_with_registrations(
            self.attempt
                .as_mut()
                .expect("an open transaction always owns its attempt"),
            &mut self.registrations,
        )
    }

    pub(super) fn rollback(&mut self, lifecycle: &LifecycleThreadAccess<'_, A>) {
        if !self.registrations.is_empty() {
            self.runtime.retain_registration_debt(
                std::mem::take(&mut self.registrations)
                    .into_iter()
                    .map(crate::registration::PendingRegistration::from)
                    .collect(),
            );
        }
        rollback_active_open(
            self.runtime,
            lifecycle,
            self.attempt.as_mut(),
            &mut self.callbacks,
        );
    }
}

impl<A: Addin> Drop for OpeningTransaction<'_, A> {
    fn drop(&mut self) {
        if self
            .attempt
            .as_ref()
            .is_some_and(OpenAttemptGuard::is_active)
        {
            // A dropped transaction must not call Excel. It is an unrecovered
            // protocol failure, so retain the fail-safe terminal state for a
            // later explicit removal/reload decision.
            self.runtime.quarantine();
        }
    }
}

pub fn open_addin_boundary<A>(
    runtime: &Runtime<A>,
    lifecycle: &LifecycleThreadAccess<'_, A>,
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
    let mut transaction = None;
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

        transaction = Some(OpeningTransaction::begin(runtime, removal_epoch)?);
        let transaction = transaction
            .as_mut()
            .expect("the open transaction was installed");
        super::retry_metadata_debt(runtime, transaction.callbacks_mut())?;
        let registrations = open_addin_inner::<A>(
            runtime,
            lifecycle,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction.callbacks_mut(),
        )?;
        transaction.stage_registrations(registrations);
        transaction.commit()
    }));

    match result {
        Ok(Ok(())) => {
            super::write_startup_log(addin_id, "xlAutoOpen succeeded");
            1
        }
        Ok(Err(error)) => {
            super::write_startup_log(addin_id, &format!("xlAutoOpen failed: {error}"));
            report_boundary_error("xlAutoOpen", &error);
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback(lifecycle);
            }
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            super::write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback(lifecycle);
            }
            0
        }
    }
}
