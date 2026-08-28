//! Operation-scoped views assembled by [`super::Runtime`].
//!
//! These views are deliberately borrowed and private to the runtime domain.
//! They make the facilities used by an operation explicit without turning
//! `Runtime` itself into a service locator available to every transaction.

use super::{AddinLifecycleAccess, Runtime};
use crate::addin::Addin;
use crate::lifecycle::{LifecycleAccess, LifecycleControl, LifecycleCoordinator};
use crate::runtime::observer::RuntimeObserver;
use crate::runtime_components::{GenerationServices, HostLedger, QuarantineVault, ReturnProtocol};
use std::sync::Arc;
use xlfn_kernel::thread_affine::{ThreadAffineInstallError, ThreadAffineSlot};

#[cfg(feature = "async")]
use crate::runtime_components::RuntimeExecutors;

/// Facilities needed by one open transaction.
///
/// The fields remain private so callers can only use the operation-specific
/// methods below. In particular, adding a new runtime-wide dependency to an
/// open transaction requires an explicit capability API change.
pub(crate) struct OpenDeps<'a, A: Addin> {
    lifecycle: &'a LifecycleCoordinator<A>,
    addin_lifecycle: &'a ThreadAffineSlot<A::LifecycleState>,
    host: &'a HostLedger,
    returns: &'a ReturnProtocol,
    #[cfg(feature = "async")]
    #[allow(
        dead_code,
        reason = "the observer projects executor state only in refinement builds"
    )]
    executors: &'a RuntimeExecutors,
    quarantine: &'a QuarantineVault<A>,
    observer: &'a RuntimeObserver,
}

impl<A: Addin> Copy for OpenDeps<'_, A> {}

impl<A: Addin> Clone for OpenDeps<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, A: Addin> OpenDeps<'a, A> {
    pub(in crate::runtime) const fn from_runtime(runtime: &'a Runtime<A>) -> Self {
        Self {
            lifecycle: &runtime.lifecycle,
            addin_lifecycle: &runtime.addin_lifecycle,
            host: &runtime.host,
            returns: &runtime.return_protocol,
            #[cfg(feature = "async")]
            executors: &runtime.executors,
            quarantine: &runtime.quarantine,
            observer: &runtime.observer,
        }
    }

    pub(in crate::runtime) fn lifecycle(&self) -> &'a LifecycleCoordinator<A> {
        self.lifecycle
    }

    pub(in crate::runtime) fn lifecycle_control(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(self.lifecycle)
    }

    pub(in crate::runtime) fn lifecycle_access(&self) -> LifecycleAccess<'_, A> {
        self.lifecycle.access()
    }

    pub(in crate::runtime) fn returns(&self) -> &'a ReturnProtocol {
        self.returns
    }

    #[allow(
        dead_code,
        reason = "open host state is projected only to the refinement observer"
    )]
    pub(in crate::runtime) fn host(&self) -> &'a HostLedger {
        self.host
    }

    #[allow(
        dead_code,
        reason = "the observer samples generation services only for formal traces"
    )]
    pub(in crate::runtime) fn generation_services_snapshot(
        &self,
    ) -> Option<Arc<GenerationServices>> {
        self.lifecycle.load_generation_services().or_else(|| {
            let control = self.lifecycle.access();
            control.retiring_services().map(Arc::clone)
        })
    }

    #[cfg(feature = "async")]
    #[allow(
        dead_code,
        reason = "the observer projects executor state only in refinement builds"
    )]
    pub(in crate::runtime) fn executors(&self) -> &'a RuntimeExecutors {
        self.executors
    }

    pub(in crate::runtime) fn quarantine(&self) -> &'a QuarantineVault<A> {
        self.quarantine
    }

    pub(in crate::runtime) fn quarantine_opening_generation(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        opening: crate::generation::OpeningGeneration<A>,
        reason: crate::runtime_components::QuarantineReason,
    ) {
        let crate::generation::OpeningGeneration {
            shared_state,
            layers,
            init_config: _,
        } = opening;
        if let Some(id) = generation {
            self.quarantine.retain_generation(
                Some(id),
                crate::generation::ExecutionGeneration {
                    id,
                    shared_state,
                    layers,
                },
                reason,
            );
        } else {
            self.quarantine
                .retain_shared_state(None, shared_state, reason);
            self.quarantine.retain_layers(None, layers, reason);
        }
    }

    pub(in crate::runtime) fn observer(&self) -> &'a RuntimeObserver {
        self.observer
    }

    pub(in crate::runtime) fn protocol_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().protocol_generation()
    }

    pub(in crate::runtime) fn merge_host(&self, journal: crate::registration::HostMutationJournal) {
        self.host.merge(journal);
    }

    pub(in crate::runtime) fn clear_metadata_debt_for_registrations(
        &self,
        registrations: &[crate::registration::RegistrationId],
    ) {
        self.host
            .clear_metadata_debt_for_registrations(registrations);
    }

    pub(in crate::runtime) fn install_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<(), ThreadAffineInstallError<A::LifecycleState>> {
        self.addin_lifecycle.install(access, state)
    }
}

/// Facilities needed by shutdown and removal transactions.
///
/// Shutdown stages receive this view instead of the aggregate `Runtime`, so
/// they can only operate on lifecycle state, host/return resources, the
/// optional async executor, quarantine, and the refinement observer. The
/// composition root remains the only place that assembles this set.
pub(crate) struct ShutdownDeps<'a, A: Addin> {
    lifecycle: &'a LifecycleCoordinator<A>,
    addin_lifecycle: &'a ThreadAffineSlot<A::LifecycleState>,
    host: &'a HostLedger,
    returns: &'a ReturnProtocol,
    #[cfg(feature = "async")]
    executors: &'a RuntimeExecutors,
    quarantine: &'a QuarantineVault<A>,
    observer: &'a RuntimeObserver,
}

impl<A: Addin> Copy for ShutdownDeps<'_, A> {}

impl<A: Addin> Clone for ShutdownDeps<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, A: Addin> ShutdownDeps<'a, A> {
    pub(in crate::runtime) fn from_runtime(runtime: &'a Runtime<A>) -> Self {
        Self {
            lifecycle: &runtime.lifecycle,
            addin_lifecycle: &runtime.addin_lifecycle,
            host: &runtime.host,
            returns: &runtime.return_protocol,
            #[cfg(feature = "async")]
            executors: &runtime.executors,
            quarantine: &runtime.quarantine,
            observer: &runtime.observer,
        }
    }

    pub(in crate::runtime) fn lifecycle(&self) -> &'a LifecycleCoordinator<A> {
        self.lifecycle
    }

    pub(in crate::runtime) fn lifecycle_control(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(self.lifecycle)
    }

    pub(in crate::runtime) fn host(&self) -> &'a HostLedger {
        self.host
    }

    #[cfg(feature = "async")]
    pub(in crate::runtime) fn async_manager(&self) -> &'a crate::async_udf::AsyncManager {
        &self.executors.async_manager
    }

    pub(in crate::runtime) fn quarantine(&self) -> &'a QuarantineVault<A> {
        self.quarantine
    }

    pub(in crate::runtime) fn observer(&self) -> &'a RuntimeObserver {
        self.observer
    }

    pub(in crate::runtime) fn protocol_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().protocol_generation()
    }

    pub(in crate::runtime) fn last_committed_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().last_committed_generation()
    }

    pub(in crate::runtime) fn with_addin_lifecycle<R>(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Result<R, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.with_mut(access, operation)
    }

    pub(in crate::runtime) fn take_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<A::LifecycleState, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.take(access)
    }

    pub(in crate::runtime) fn has_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<bool, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.has_value(access)
    }

    pub(in crate::runtime) fn release_empty_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<(), xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.release_empty_binding(access)
    }

    pub(in crate::runtime) fn wait_for_return_quiescence(
        &self,
    ) -> crate::XllResult<crate::shutdown::ReturnsQuiescent> {
        crate::shutdown::wait_for_return_quiescence(self.returns)
    }

    pub(in crate::runtime) fn close_subscriptions(
        &self,
    ) -> crate::XllResult<crate::shutdown::SubscriptionsStopped> {
        #[cfg(not(feature = "rtd"))]
        {
            Ok(crate::excel_rtd::stopped_subscriptions(
                self.protocol_generation(),
            ))
        }
        #[cfg(feature = "rtd")]
        {
            let services = self.lifecycle.load_generation_services().or_else(|| {
                let control = self.lifecycle.access();
                control.retiring_services().map(std::sync::Arc::clone)
            });
            let Some(services) = services else {
                #[cfg(test)]
                {
                    return Ok(crate::excel_rtd::stopped_subscriptions(
                        self.protocol_generation(),
                    ));
                }
                #[cfg(not(test))]
                {
                    return Err(crate::XllError::Closing);
                }
            };
            services.close_subscriptions(self.protocol_generation())
        }
    }

    pub(in crate::runtime) fn seal_generation_services(
        &self,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<crate::runtime_components::SealedGenerationServices> {
        let generation = self.protocol_generation();
        let services = self.lifecycle.load_generation_services().or_else(|| {
            let control = self.lifecycle.access();
            control.retiring_services().map(std::sync::Arc::clone)
        });
        let Some(services) = services else {
            return Ok(crate::runtime_components::SealedGenerationServices::empty(
                generation,
                subscriptions_stopped,
            ));
        };
        services.seal(generation, subscriptions_stopped)
    }

    pub(in crate::runtime) fn shutdown_handle_topics(&self) -> crate::XllResult<()> {
        let services = self.lifecycle.load_generation_services().or_else(|| {
            let control = self.lifecycle.access();
            control.retiring_services().map(std::sync::Arc::clone)
        });
        let Some(services) = services else {
            return Ok(());
        };
        services.shutdown_handle_topics()
    }

    pub(in crate::runtime) fn take_generation_for_shutdown(
        &self,
    ) -> Option<crate::generation::ShutdownGeneration<A>> {
        self.lifecycle.take_generation_for_shutdown()
    }

    pub(in crate::runtime) fn quarantine_state(&self) {
        let lifecycle = self.lifecycle_control();
        let mut control = lifecycle.access();
        self.returns.close_admission();
        lifecycle.quarantine_state(&mut control);
    }

    #[cfg(test)]
    pub(in crate::runtime) fn release_test_module_lease(&self) {
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}
