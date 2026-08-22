//! Transaction ownership for one logical open attempt.

use super::{open_addin_inner, report_boundary_error, rollback_active_open};
use crate::host_callback::HostCallbackSession;
use crate::runtime::OpenAttemptGuard;
use crate::{Addin, AddinId, BuildInfo, RegistrationDescriptor, Runtime, XllError, XllResult};

/// Owns one logical open attempt, including the callback session that can
/// undo host mutations made by that attempt. The caller must explicitly call
/// [`Self::finish`] or [`Self::rollback`]; dropping an active transaction only
/// quarantines the runtime and never performs implicit callback cleanup.
pub(super) struct OpenTransaction<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
    callbacks: HostCallbackSession,
    attempt: Option<OpenAttemptGuard<'runtime, A>>,
}

impl<'runtime, A: Addin> OpenTransaction<'runtime, A> {
    pub(super) fn begin(
        runtime: &'runtime Runtime<A>,
        removal_epoch: crate::generation::RemovalEpoch,
    ) -> XllResult<Self> {
        Ok(Self {
            runtime,
            callbacks: HostCallbackSession::new(),
            attempt: Some(runtime.begin_open_if_epoch(removal_epoch)?),
        })
    }

    pub(super) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.callbacks
    }

    pub(super) fn finish(&mut self, registrations: Vec<crate::RegistrationId>) -> XllResult<()> {
        self.runtime.finish_open(
            self.attempt
                .as_mut()
                .expect("an open transaction always owns its attempt"),
            registrations,
        )
    }

    pub(super) fn rollback(&mut self) {
        rollback_active_open(self.runtime, self.attempt.as_mut(), &mut self.callbacks);
    }
}

impl<A: Addin> Drop for OpenTransaction<'_, A> {
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
        if runtime.phase() == crate::LifecyclePhase::OpenRollbackPending {
            let mut callbacks = HostCallbackSession::new();
            let outcome = super::rollback_open::<A>(
                runtime,
                &mut callbacks,
                super::active_runtime_generation(runtime),
            );
            if !outcome.unload_safe() {
                let error = XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OPEN_ROLLBACK_PENDING,
                };
                report_boundary_error("xlAutoOpen pending rollback", &error);
                super::quarantine_runtime(runtime);
                return Err(error);
            }
        }

        if runtime.removal_epoch() != removal_epoch {
            return Err(XllError::Closing);
        }

        transaction = Some(OpenTransaction::begin(runtime, removal_epoch)?);
        let transaction = transaction
            .as_mut()
            .expect("the open transaction was installed");
        super::retry_metadata_debt(runtime, transaction.callbacks_mut())?;
        let registrations = open_addin_inner::<A>(
            runtime,
            BuildInfo::new(addin_id.clone(), version, target),
            descriptors,
            transaction.callbacks_mut(),
        )?;
        transaction.finish(registrations)
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
                transaction.rollback();
            }
            0
        }
        Err(_) => {
            let error = XllError::Panic;
            super::write_startup_log(addin_id, "xlAutoOpen failed: panic at boundary");
            report_boundary_error("xlAutoOpen", &error);
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback();
            }
            0
        }
    }
}
