use crate::generation::{OpenAttemptId, OpeningGeneration};
use crate::host_callback::HostCallbackSession;
use crate::lifecycle::LifecycleAccess;
#[cfg(test)]
use crate::lifecycle::OpenFailureDisposition;
use crate::registration::{HostMutationJournal, RegistrationId};
use crate::runtime::AddinLifecycleAccess;
use crate::runtime::capabilities::OpenDeps;
use crate::runtime_components::GenerationServices;
use crate::{XllError, XllResult};
use std::sync::Arc;
use xlfn_kernel::thread_affine::ThreadAffineError;

/// A state payload in the open transaction protocol.
pub(crate) trait OpeningState<A: crate::Addin>: Sized {
    fn attempt_id(&self) -> OpenAttemptId;
    fn abandon(self, deps: &OpenDeps<'_, A>);
}

/// The only common payload shared by every open transaction state.
pub(crate) struct OpenCore {
    attempt_id: OpenAttemptId,
    module_opening: crate::module_runtime::ModuleOpening,
}

impl OpenCore {
    fn new(
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
    ) -> Self {
        Self {
            attempt_id,
            module_opening,
        }
    }

    fn attempt_id(&self) -> OpenAttemptId {
        self.attempt_id
    }

    pub(crate) fn into_parts(self) -> (OpenAttemptId, crate::module_runtime::ModuleOpening) {
        (self.attempt_id, self.module_opening)
    }
}

/// The open protocol has begun, but no host callback session is attached yet.
pub(crate) struct Begun {
    core: OpenCore,
}

/// Host callback and registration journal ownership is now attached.
pub(crate) struct HostAttached {
    core: OpenCore,
    host: HostOpeningState,
}

/// `Addin::open` succeeded and its thread-affine lifecycle state is owned by
/// the transaction.
pub(crate) struct Initialized<A: crate::Addin> {
    core: OpenCore,
    host: HostOpeningState,
    lifecycle: A::LifecycleState,
}

/// The generation is staged in canonical lifecycle state, while the
/// thread-affine lifecycle payload remains owned by the transaction.
pub(crate) struct GenerationStaged<A: crate::Addin> {
    core: OpenCore,
    host: HostOpeningState,
    lifecycle: A::LifecycleState,
}

/// The lifecycle payload has been transferred to the runtime's thread-affine
/// slot. The transaction still owns the module and host capabilities.
pub(crate) struct LifecycleInstalled {
    core: OpenCore,
    host: HostOpeningState,
}

/// All mandatory open steps are complete and the transaction may commit.
pub(crate) struct HostMutated {
    core: OpenCore,
    host: HostOpeningState,
}

pub(crate) struct HostOpeningState {
    callbacks: HostCallbackSession,
    journal: HostMutationJournal,
}

impl HostOpeningState {
    fn new() -> Self {
        Self {
            callbacks: HostCallbackSession::new(),
            journal: HostMutationJournal::default(),
        }
    }

    pub(crate) fn into_parts(self) -> (HostCallbackSession, HostMutationJournal) {
        (self.callbacks, self.journal)
    }

    fn stage_registrations(&mut self, registrations: Vec<RegistrationId>) {
        self.journal.pending_registrations = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
    }
}

/// Lifecycle ownership while an opening transaction is being rolled back.
/// The enum is a recovery projection, not an independent state machine: the
/// production transaction states above determine which variant can be made.
pub(crate) enum LifecycleOwnership<A: crate::Addin> {
    Owned(A::LifecycleState),
    Installed,
}

pub(crate) struct RollbackParts<A: crate::Addin> {
    core: OpenCore,
    host: HostOpeningState,
    lifecycle: LifecycleOwnership<A>,
}

impl<A: crate::Addin> RollbackParts<A> {
    pub(crate) fn into_parts(self) -> (OpenCore, HostOpeningState, LifecycleOwnership<A>) {
        (self.core, self.host, self.lifecycle)
    }
}

/// States that still own the host-side rollback session.
pub(crate) trait RollbackState<A: crate::Addin>: OpeningState<A> {
    fn into_rollback_parts(self) -> RollbackParts<A>;
}

fn forget_untrusted_lifecycle_state<A: crate::Addin>(state: A::LifecycleState) {
    #[allow(
        clippy::mem_forget,
        reason = "an abandoned open transaction must not run untrusted lifecycle destruction"
    )]
    std::mem::forget(state);
}

fn abandon_open<A: crate::Addin>(
    deps: &OpenDeps<'_, A>,
    core: OpenCore,
    lifecycle: LifecycleOwnership<A>,
) {
    let (_, module_opening) = core.into_parts();
    deps.lifecycle_control()
        .complete_open_abort(module_opening.rollback(|| {}));
    let control_api = deps.lifecycle_control();
    let mut control = control_api.access();
    control_api.quarantine_state(&mut control);
    drop(control);
    control_api.notify_all();
    if let LifecycleOwnership::Owned(state) = lifecycle {
        forget_untrusted_lifecycle_state::<A>(state);
    }
}

impl<A: crate::Addin> OpeningState<A> for Begun {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for HostAttached {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for Initialized<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self {
            core,
            host: _,
            lifecycle,
        } = self;
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin> OpeningState<A> for GenerationStaged<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self {
            core,
            host: _,
            lifecycle,
        } = self;
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin> OpeningState<A> for LifecycleInstalled {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for HostMutated {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> RollbackState<A> for HostAttached {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core,
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for Initialized<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self {
            core,
            host,
            lifecycle,
        } = self;
        RollbackParts {
            core,
            host,
            lifecycle: LifecycleOwnership::Owned(lifecycle),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for GenerationStaged<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self {
            core,
            host,
            lifecycle,
        } = self;
        RollbackParts {
            core,
            host,
            lifecycle: LifecycleOwnership::Owned(lifecycle),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for LifecycleInstalled {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core,
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for HostMutated {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core,
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

pub(crate) trait HasHost {
    fn host_mut(&mut self) -> &mut HostOpeningState;
}

impl HasHost for HostAttached {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for Initialized<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for GenerationStaged<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl HasHost for LifecycleInstalled {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl HasHost for HostMutated {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

pub(crate) struct OpeningTxn<'runtime, A: crate::Addin, S: OpeningState<A>> {
    deps: OpenDeps<'runtime, A>,
    state: Option<S>,
}

type OpeningStageFailure<'runtime, A> = (
    XllError,
    Box<OpeningTxn<'runtime, A, Initialized<A>>>,
    Box<OpeningGeneration<A>>,
);

type LifecycleInstallFailure<'runtime, A> = (
    ThreadAffineError,
    Box<OpeningTxn<'runtime, A, GenerationStaged<A>>>,
);

impl<'runtime, A: crate::Addin, S: OpeningState<A>> OpeningTxn<'runtime, A, S> {
    fn new_state(deps: OpenDeps<'runtime, A>, state: S) -> Self {
        Self {
            deps,
            state: Some(state),
        }
    }

    fn take_state(&mut self) -> S {
        self.state
            .take()
            .expect("opening transaction state already consumed")
    }

    pub(crate) fn attempt_id(&self) -> OpenAttemptId {
        match &self.state {
            Some(state) => state.attempt_id(),
            None => panic!("opening transaction state already consumed"),
        }
    }

    pub(crate) fn deps(&self) -> OpenDeps<'runtime, A> {
        self.deps
    }

    pub(crate) fn into_rollback_parts(self) -> RollbackParts<A>
    where
        S: RollbackState<A>,
    {
        let mut transaction = self;
        let state = transaction.take_state();
        state.into_rollback_parts()
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Begun> {
    pub(crate) fn new_begun(
        deps: OpenDeps<'runtime, A>,
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
    ) -> Self {
        Self::new_state(
            deps,
            Begun {
                core: OpenCore::new(attempt_id, module_opening),
            },
        )
    }

    pub(crate) fn attach_host(self) -> OpeningTxn<'runtime, A, HostAttached> {
        let mut transaction = self;
        let Begun { core } = transaction.take_state();
        OpeningTxn::new_state(
            transaction.deps,
            HostAttached {
                core,
                host: HostOpeningState::new(),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let mut transaction = self;
        let attempt_id = transaction.attempt_id();
        let disposition =
            crate::runtime::orchestration::LifecycleOrchestrator::new(transaction.deps)
                .mark_open_failed(attempt_id);
        let Begun { core } = transaction.take_state();
        let (_, module_opening) = core.into_parts();
        transaction
            .deps
            .lifecycle_control()
            .complete_open_abort(module_opening.rollback(|| {}));
        disposition
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostAttached> {
    pub(crate) fn initialized(
        self,
        lifecycle: A::LifecycleState,
    ) -> OpeningTxn<'runtime, A, Initialized<A>> {
        let mut transaction = self;
        let HostAttached { core, host } = transaction.take_state();
        OpeningTxn::new_state(
            transaction.deps,
            Initialized {
                core,
                host,
                lifecycle,
            },
        )
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Initialized<A>> {
    pub(crate) fn stage_opening_generation(
        self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTxn<'runtime, A, GenerationStaged<A>>, OpeningStageFailure<'runtime, A>>
    {
        let mut transaction = self;
        let result = transaction
            .deps
            .lifecycle_control()
            .stage_opening_generation(transaction.attempt_id(), opening);
        match result {
            Ok(()) => {
                let Initialized {
                    core,
                    host,
                    lifecycle,
                } = transaction.take_state();
                Ok(OpeningTxn::new_state(
                    transaction.deps,
                    GenerationStaged {
                        core,
                        host,
                        lifecycle,
                    },
                ))
            }
            Err((error, opening)) => Err((error, Box::new(transaction), Box::new(opening))),
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, GenerationStaged<A>> {
    pub(crate) fn install_lifecycle(
        self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<OpeningTxn<'runtime, A, LifecycleInstalled>, LifecycleInstallFailure<'runtime, A>>
    {
        let mut transaction = self;
        let GenerationStaged {
            core,
            host,
            lifecycle,
        } = transaction.take_state();
        match transaction.deps.install_addin_lifecycle(access, lifecycle) {
            Ok(()) => Ok(OpeningTxn::new_state(
                transaction.deps,
                LifecycleInstalled { core, host },
            )),
            Err(error) => {
                transaction.state = Some(GenerationStaged {
                    core,
                    host,
                    lifecycle: error.value,
                });
                Err((error.reason, Box::new(transaction)))
            }
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, LifecycleInstalled> {
    pub(crate) fn stage_host_mutations(
        self,
        registrations: Vec<RegistrationId>,
    ) -> OpeningTxn<'runtime, A, HostMutated> {
        let mut transaction = self;
        let LifecycleInstalled { core, mut host } = transaction.take_state();
        host.stage_registrations(registrations);
        OpeningTxn::new_state(transaction.deps, HostMutated { core, host })
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let mut transaction = self;
        let attempt_id = transaction.attempt_id();
        let disposition =
            crate::runtime::orchestration::LifecycleOrchestrator::new(transaction.deps)
                .mark_open_failed(attempt_id);
        let LifecycleInstalled { core, host: _ } = transaction.take_state();
        let (_, module_opening) = core.into_parts();
        transaction
            .deps
            .lifecycle_control()
            .complete_open_abort(module_opening.rollback(|| {}));
        disposition
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn finish_in_place(&mut self, registrations: Vec<RegistrationId>) -> XllResult<()> {
        let state = self.take_state();
        OpeningTxn::new_state(self.deps, state)
            .stage_host_mutations(registrations)
            .commit()
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostMutated> {
    pub(crate) fn commit(self) -> XllResult<()> {
        let mut transaction = self;
        validate_commit_preconditions(&transaction)?;
        let HostMutated { core, host } = transaction.take_state();
        let (_callbacks, mut journal) = host.into_parts();
        commit_inner(transaction.deps, core, &mut journal)
    }
}

impl<'runtime, A: crate::Addin, S> OpeningTxn<'runtime, A, S>
where
    S: OpeningState<A> + HasHost,
{
    pub(crate) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self
            .state
            .as_mut()
            .expect("opening transaction state already consumed")
            .host_mut()
            .callbacks
    }

    #[cfg(feature = "async")]
    pub(crate) fn stage_events(
        &mut self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.state
            .as_mut()
            .expect("opening transaction state already consumed")
            .host_mut()
            .journal
            .pending_events = registrations;
    }

    pub(crate) fn retain_journal(&mut self, journal: HostMutationJournal) {
        self.state
            .as_mut()
            .expect("opening transaction state already consumed")
            .host_mut()
            .journal
            .merge(journal);
    }
}

fn validate_commit_preconditions<A: crate::Addin, S: OpeningState<A>>(
    transaction: &OpeningTxn<'_, A, S>,
) -> XllResult<()> {
    let control = transaction.deps.lifecycle_access();
    transaction
        .deps
        .lifecycle_control()
        .validate_open_attempt(&control, transaction.attempt_id())
}

fn commit_inner<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    core: OpenCore,
    journal: &mut HostMutationJournal,
) -> XllResult<()> {
    let (attempt_id, module_opening) = core.into_parts();
    let mut control = deps.lifecycle_access();
    let registration_ids = journal
        .pending_registrations
        .iter()
        .map(|entry| entry.registration)
        .collect::<Vec<_>>();
    deps.clear_metadata_debt_for_registrations(&registration_ids);
    deps.merge_host(std::mem::take(journal));

    if control.phase() == crate::lifecycle::LifecyclePhase::Opening {
        let mut module_opening = Some(module_opening);
        let ingress = crate::module_runtime::ingress();
        let generation = attempt_id.into_runtime_generation();
        let result = ingress
            .complete_open(|| {
                publish_generation(deps, attempt_id, &mut control, &mut module_opening)?;
                deps.lifecycle_control()
                    .finish_open_state(&mut control, generation)?;
                if control.phase() != crate::lifecycle::LifecyclePhase::Open
                    || control.last_committed_generation() != Some(generation)
                    || control.open_attempt().is_some()
                {
                    crate::boundary::fail_stop_invariant(
                        "xlAutoOpen commit postcondition",
                        &XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                        },
                    );
                }
                Ok::<(), XllError>(())
            })
            .unwrap_or_else(|_| opening_publication_lost());

        match result {
            Ok(()) => {
                deps.observer().commit_open(&deps, attempt_id, generation);
                deps.lifecycle().notify_all();
                Ok(())
            }
            Err(error) => {
                recover_uncommitted_module(deps, &mut module_opening, &mut control);
                drop(control);
                let lifecycle = deps.lifecycle_control();
                let mut control = lifecycle.access();
                lifecycle.quarantine_state(&mut control);
                drop(control);
                lifecycle.notify_all();
                Err(error)
            }
        }
    } else {
        let authority = deps.lifecycle_control();
        let closing = module_opening.rollback(|| {});
        authority.complete_open_abort_locked(&mut control, closing);
        deps.lifecycle().notify_all();
        drop(control);
        deps.observer().reject_open(attempt_id);
        Err(XllError::Closing)
    }
}

fn publish_generation<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    attempt_id: OpenAttemptId,
    control: &mut LifecycleAccess<'_, A>,
    module_opening: &mut Option<crate::module_runtime::ModuleOpening>,
) -> XllResult<()> {
    let generation = attempt_id.into_runtime_generation();
    let config = control.opening_config().ok_or(XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
    })?;
    #[cfg(feature = "rtd")]
    let subscription_host = Some(crate::excel_rtd::RtdSubscriptionHost::production(
        crate::module_runtime::ingress(),
    ));
    #[cfg(not(feature = "rtd"))]
    let subscription_host = None;
    let services =
        GenerationServices::arm_generation(generation, config, subscription_host)?.commit();
    let module_epoch = module_opening
        .take()
        .expect("open transaction owns its module opening authority")
        .commit();
    match deps.lifecycle_control().publish_generation_state(
        control,
        generation,
        Arc::clone(&services),
        module_epoch,
    ) {
        Ok(()) => Ok(()),
        Err(failure) => {
            let (error, opening, module_epoch) = *failure;
            services.disarm_or_abort();
            if let Some(opening) = opening {
                let crate::generation::OpeningGeneration {
                    shared_state,
                    layers,
                    init_config: _,
                } = opening;
                deps.quarantine().retain_generation(
                    Some(generation),
                    crate::generation::ExecutionGeneration {
                        id: generation,
                        shared_state,
                        layers,
                    },
                    crate::runtime_components::QuarantineReason::OpenStateInvariant,
                );
            }
            let closing = module_epoch.begin_close(|| {});
            deps.lifecycle_control()
                .complete_open_abort_locked(control, closing);
            Err(error)
        }
    }
}

fn recover_uncommitted_module<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    module_opening: &mut Option<crate::module_runtime::ModuleOpening>,
    control: &mut LifecycleAccess<'_, A>,
) {
    if let Some(module_opening) = module_opening.take() {
        deps.lifecycle_control()
            .complete_open_abort_locked(control, module_opening.rollback(|| {}));
    }
}

impl<A: crate::Addin, S: OpeningState<A>> Drop for OpeningTxn<'_, A, S> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state.abandon(&self.deps);
        }
    }
}

#[cold]
fn opening_publication_lost() -> ! {
    crate::boundary::fail_stop_invariant(
        "xlAutoOpen opening publication",
        &XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
        },
    )
}
