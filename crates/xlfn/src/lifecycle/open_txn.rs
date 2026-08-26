use crate::generation::{OpenAttemptId, OpeningGeneration};
use crate::host_callback::HostCallbackSession;
use crate::lifecycle::OpenFailureDisposition;
use crate::registration::{HostMutationJournal, RegistrationId};
use crate::runtime::{AddinLifecycleAccess, Runtime};
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use xlfn_kernel::thread_affine::ThreadAffineError;

pub(crate) struct OpenAttemptBegun;

pub(crate) struct OpenGenerationStaged;

pub(crate) struct HostMutated;

pub(crate) type OpeningStageFailure<'runtime, A, Host = NoHost> = (
    XllError,
    Box<OpeningTxn<'runtime, A, OpenAttemptBegun, Host>>,
    Box<OpeningGeneration<A>>,
);

pub(crate) struct NoHost;

pub(crate) struct HostOpeningState {
    callbacks: HostCallbackSession,
    journal: HostMutationJournal,
}

pub(crate) struct OpeningTxn<'runtime, A: crate::Addin, Stage, Host = NoHost> {
    runtime: &'runtime Runtime<A>,
    attempt_id: OpenAttemptId,
    module_opening: Option<crate::module_runtime::ModuleOpening>,
    host: Option<Host>,
    lifecycle_state: Option<A::LifecycleState>,
    _stage: PhantomData<fn() -> Stage>,
}

impl<'runtime, A: crate::Addin, Stage, Host> OpeningTxn<'runtime, A, Stage, Host> {
    pub(crate) const fn attempt_id(&self) -> OpenAttemptId {
        self.attempt_id
    }

    pub(crate) fn runtime(&self) -> &'runtime Runtime<A> {
        self.runtime
    }

    pub(crate) fn fail(&mut self) -> OpenFailureDisposition {
        let disposition =
            crate::lifecycle::LifecycleAuthority::new(self.runtime).fail_open(self.attempt_id);
        if let Some(module_opening) = self.module_opening.take() {
            crate::lifecycle::LifecycleAuthority::new(self.runtime)
                .install_module_closing(module_opening.rollback(|| {}));
        }
        disposition
    }

    pub(crate) fn with_lifecycle_state(mut self, state: A::LifecycleState) -> Self {
        debug_assert!(
            self.lifecycle_state.is_none(),
            "an opening transaction receives one lifecycle state"
        );
        self.lifecycle_state = Some(state);
        self
    }

    pub(crate) fn take_lifecycle_state(&mut self) -> Option<A::LifecycleState> {
        self.lifecycle_state.take()
    }

    pub(crate) fn install_lifecycle(
        mut self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<Self, (ThreadAffineError, Self)> {
        let Some(state) = self.lifecycle_state.take() else {
            return Ok(self);
        };
        match self.runtime.install_addin_lifecycle(access, state) {
            Ok(()) => Ok(self),
            Err(error) => {
                self.lifecycle_state = Some(error.value);
                Err((error.reason, self))
            }
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, OpenAttemptBegun, NoHost> {
    pub(crate) fn new_begun(
        runtime: &'runtime Runtime<A>,
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
    ) -> Self {
        Self {
            runtime,
            attempt_id,
            module_opening: Some(module_opening),
            host: Some(NoHost),
            lifecycle_state: None,
            _stage: PhantomData,
        }
    }
}

impl<'runtime, A: crate::Addin, Stage> OpeningTxn<'runtime, A, Stage, NoHost> {
    pub(crate) fn attach_host(mut self) -> OpeningTxn<'runtime, A, Stage, HostOpeningState> {
        OpeningTxn {
            runtime: self.runtime,
            attempt_id: self.attempt_id,
            module_opening: self.module_opening.take(),
            host: Some(HostOpeningState {
                callbacks: HostCallbackSession::new(),
                journal: HostMutationJournal::default(),
            }),
            lifecycle_state: self.lifecycle_state.take(),
            _stage: PhantomData,
        }
    }
}

impl<'runtime, A: crate::Addin, Host> OpeningTxn<'runtime, A, OpenAttemptBegun, Host> {
    pub(crate) fn stage(
        mut self,
        opening: OpeningGeneration<A>,
    ) -> Result<
        OpeningTxn<'runtime, A, OpenGenerationStaged, Host>,
        OpeningStageFailure<'runtime, A, Host>,
    > {
        let result = crate::lifecycle::LifecycleAuthority::new(self.runtime)
            .stage_opening_generation(self.attempt_id, opening);
        match result {
            Ok(()) => {
                let module_opening = self
                    .module_opening
                    .take()
                    .expect("an open attempt owns the module token before staging");
                Ok(OpeningTxn {
                    runtime: self.runtime,
                    attempt_id: self.attempt_id,
                    module_opening: Some(module_opening),
                    host: self.host.take(),
                    lifecycle_state: self.lifecycle_state.take(),
                    _stage: PhantomData,
                })
            }
            Err((error, opening)) => Err((error, Box::new(self), Box::new(opening))),
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, OpenGenerationStaged, HostOpeningState> {
    fn stage_registrations(&mut self, registrations: Vec<RegistrationId>) {
        self.host
            .as_mut()
            .expect("a host transaction owns its opening state")
            .journal
            .pending_registrations = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
    }

    pub(crate) fn stage_host_mutations(
        mut self,
        registrations: Vec<RegistrationId>,
    ) -> OpeningTxn<'runtime, A, HostMutated, HostOpeningState> {
        self.stage_registrations(registrations);
        OpeningTxn {
            runtime: self.runtime,
            attempt_id: self.attempt_id,
            module_opening: self.module_opening.take(),
            host: self.host.take(),
            lifecycle_state: self.lifecycle_state.take(),
            _stage: PhantomData,
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostMutated, HostOpeningState> {
    fn journal_mut(&mut self) -> &mut HostMutationJournal {
        &mut self
            .host
            .as_mut()
            .expect("a host transaction owns its opening state")
            .journal
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        let Some(module_opening) = self.module_opening.take() else {
            return Err(XllError::Closing);
        };
        crate::lifecycle::LifecycleAuthority::new(self.runtime).commit_open(
            self.attempt_id,
            module_opening,
            self.journal_mut(),
        )
    }
}

impl<'runtime, A: crate::Addin, Stage> OpeningTxn<'runtime, A, Stage, NoHost> {
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn commit_in_place(&mut self, registrations: Vec<RegistrationId>) -> XllResult<()> {
        let mut journal = HostMutationJournal {
            pending_registrations: registrations
                .into_iter()
                .map(crate::registration::PendingRegistration::from)
                .collect(),
            ..Default::default()
        };
        let Some(module_opening) = self.module_opening.take() else {
            return Err(XllError::Closing);
        };
        crate::lifecycle::LifecycleAuthority::new(self.runtime).commit_open(
            self.attempt_id,
            module_opening,
            &mut journal,
        )
    }
}

impl<'runtime, A: crate::Addin, Stage> OpeningTxn<'runtime, A, Stage, HostOpeningState> {
    pub(crate) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self
            .host
            .as_mut()
            .expect("a host transaction owns its opening state")
            .callbacks
    }

    #[cfg(feature = "async")]
    pub(crate) fn stage_events(
        &mut self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.host
            .as_mut()
            .expect("a host transaction owns its opening state")
            .journal
            .pending_events = registrations;
    }

    pub(crate) fn retain_journal(&mut self, journal: HostMutationJournal) {
        self.host
            .as_mut()
            .expect("a host transaction owns its opening state")
            .journal
            .merge(journal);
    }

    pub(crate) fn take_journal(&mut self) -> HostMutationJournal {
        std::mem::take(
            &mut self
                .host
                .as_mut()
                .expect("a host transaction owns its opening state")
                .journal,
        )
    }
}

impl<A: crate::Addin, Stage, Host> Drop for OpeningTxn<'_, A, Stage, Host> {
    fn drop(&mut self) {
        if let Some(module_opening) = self.module_opening.take() {
            crate::lifecycle::LifecycleAuthority::new(self.runtime)
                .install_module_closing(module_opening.rollback(|| {}));
        } else {
            if let Some(lifecycle_state) = self.lifecycle_state.take() {
                #[allow(
                    clippy::mem_forget,
                    reason = "an abandoned open transaction must not run untrusted lifecycle destruction during unwind"
                )]
                std::mem::forget(lifecycle_state);
            }
            return;
        }
        // Lifecycle rollback is owned by OpeningTxn and must be explicit.
        // Dropping any unfinished stage can only enter the fail-safe state;
        // Drop never invokes host callbacks or resource cleanup.
        self.runtime.lifecycle_runtime().quarantine();
        if let Some(lifecycle_state) = self.lifecycle_state.take() {
            #[allow(
                clippy::mem_forget,
                reason = "an abandoned open transaction must not run untrusted lifecycle destruction during unwind"
            )]
            std::mem::forget(lifecycle_state);
        }
    }
}
