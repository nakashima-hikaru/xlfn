//! Residency release authority is scoped to a lifecycle observation held locked.
//! Acquisition owners retain on Drop/forget; only a failed invocation or a
//! matching host-close boundary can issue an explicit release attempt.
use super::{Runtime, UnloadPolicy};
use crate::Addin;
use crate::XllResult;
use crate::boundary::host::HostCloseBoundary;
#[cfg(test)]
use crate::lifecycle::HostLifecycleIntent;
use crate::lifecycle::{CompletedRemovalIntent, LifecycleAccess, LifecyclePhase};
use crate::runtime_components::{
    ModuleResidency, ResidencyAcquisition, ResidencyLeaseId, ResidencyReleaseOutcome,
};

pub(crate) struct ResidencyRelease<'a, A: Addin> {
    module: &'a ModuleResidency,
    reason: ReleaseReason<'a, A>,
}

enum ReleaseReason<'a, A: Addin> {
    Closed {
        _boundary: &'a HostCloseBoundary<'a>,
        completed: CompletedRemovalIntent<'a, A>,
    },
    FailedOpen {
        id: ResidencyLeaseId,
        // Retains the exact failed invocation's locked classification.
        _lifecycle: LifecycleAccess<'a, A>,
    },
}

impl<A: Addin> ResidencyRelease<'_, A> {
    pub(crate) fn module(&self) -> &ModuleResidency {
        self.module
    }
    pub(crate) fn acquisition(&self) -> Option<&ResidencyLeaseId> {
        match &self.reason {
            ReleaseReason::Closed { .. } => None,
            ReleaseReason::FailedOpen { id, .. } => Some(id),
        }
    }
    fn release(self) -> XllResult<ResidencyReleaseOutcome> {
        let outcome = ModuleResidency::release_authorized(&self)?;
        if let ReleaseReason::Closed { completed, .. } = self.reason {
            completed.clear();
        }
        Ok(outcome)
    }
}

/// The newly acquired reference never releases automatically. This owner
/// determines the result of its own invocation instead of accepting a caller's
/// bool or an unrelated runtime at the release boundary.
#[must_use]
pub(crate) struct OpenResidency<'a, A: Addin, P: UnloadPolicy<A>> {
    runtime: &'a Runtime<A, P>,
    acquisition: ResidencyAcquisition,
    next_open_sequence: u64,
}

impl<'a, A: Addin, P: UnloadPolicy<A>> OpenResidency<'a, A, P> {
    pub(crate) fn acquire(runtime: &'a Runtime<A, P>, anchor: *const ()) -> XllResult<Self> {
        let control = runtime.lifecycle.access();
        let acquisition = runtime.residency.ensure(anchor)?;
        let next_open_sequence = control.next_open_sequence();
        Ok(Self {
            runtime,
            acquisition,
            next_open_sequence,
        })
    }

    /// Metadata preparation failed before this owner entered application code.
    pub(crate) fn preparation_failed(self) {
        let expected = self.next_open_sequence;
        self.release_failed(expected);
    }

    pub(crate) fn open(
        self,
        id: &crate::diagnostics::AddinId,
        version: &'static str,
        target: &'static str,
        descriptors: &[crate::registration::RegistrationDescriptor],
    ) -> i32 {
        let outcome = crate::boundary::host::host_auto_open_observed(
            self.runtime,
            id,
            version,
            target,
            descriptors,
        );
        let result = outcome.result();
        if result == 0 {
            match outcome.attempt() {
                None => {
                    let expected = self.next_open_sequence;
                    self.release_failed(expected);
                }
                Some(attempt) if attempt.get() == self.next_open_sequence => {
                    if let Some(expected) = attempt.get().checked_add(1) {
                        self.release_failed(expected);
                    }
                }
                Some(_) => {} // An intervening invocation advanced the issuer.
            }
        }
        result
    }

    fn release_failed(self, expected_sequence: u64) {
        let ResidencyAcquisition::NewlyAcquired(id) = self.acquisition else {
            return;
        };
        if !P::PHYSICAL {
            return;
        }
        let control = self.runtime.lifecycle.access();
        if control.phase() != LifecyclePhase::Closed
            || control.open_attempt().is_some()
            || control.removal_attempt().is_some()
            || control.next_open_sequence() != expected_sequence
        {
            return;
        }
        let permission = ResidencyRelease {
            module: &self.runtime.residency,
            reason: ReleaseReason::FailedOpen {
                id,
                _lifecycle: control,
            },
        };
        if let Err(error) = permission.release() {
            crate::diagnostics::report_no_unwind("xlAutoOpen module residency release", &error);
            self.runtime.quarantine_runtime();
        }
    }
}

impl<A: Addin, P: UnloadPolicy<A>> Runtime<A, P> {
    pub(crate) fn release_closed_residency(
        &self,
        boundary: &HostCloseBoundary<'_>,
    ) -> XllResult<Option<ResidencyReleaseOutcome>> {
        if !P::PHYSICAL || !boundary.belongs_to(self) {
            return Ok(None);
        }
        let control = self.lifecycle.access();
        let Ok(completed) = control.into_completed_removal_intent() else {
            return Ok(None);
        };
        ResidencyRelease {
            module: &self.residency,
            reason: ReleaseReason::Closed {
                _boundary: boundary,
                completed,
            },
        }
        .release()
        .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addin::{OpenContext, Opened, PhysicallyUnloadableAddin};
    use crate::runtime::PhysicalUnload;

    struct Probe;
    impl Addin for Probe {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = ();
        type LifecycleState = ();
        type Layers = ();
        type Error = crate::XllError;
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;
        fn open(_: &OpenContext<Self>) -> crate::addin::OpenResult<Self> {
            Ok(Opened::new(()))
        }
    }
    #[allow(unsafe_code, reason = "The probe has no executable sources")]
    // SAFETY: all state is unit, with no user worker or callback ownership.
    unsafe impl PhysicallyUnloadableAddin for Probe {}

    fn anchor() {}
    fn physical() -> Runtime<Probe, PhysicalUnload> {
        Runtime::new_with_physical_unload()
    }

    #[test]
    fn only_new_physical_acquisition_can_release_after_preparation_failure() {
        let runtime = physical();
        let first = OpenResidency::acquire(&runtime, anchor as *const ()).unwrap();
        let existing = OpenResidency::acquire(&runtime, anchor as *const ()).unwrap();
        existing.preparation_failed();
        assert!(runtime.module_residency_held());
        first.preparation_failed();
        assert!(!runtime.module_residency_held());
        let logical = Runtime::<Probe>::new();
        OpenResidency::acquire(&logical, anchor as *const ())
            .unwrap()
            .preparation_failed();
        assert!(logical.module_residency_held());
    }

    #[test]
    #[allow(
        clippy::forget_non_drop,
        reason = "Exercise safe forgetting of an acquisition owner; it must retain residency"
    )]
    fn dropped_or_forgotten_acquisition_retains_without_certifying_cleanup() {
        let runtime = physical();
        drop(OpenResidency::acquire(&runtime, anchor as *const ()).unwrap());
        assert!(runtime.module_residency_held());
        let other = physical();
        std::mem::forget(OpenResidency::acquire(&other, anchor as *const ()).unwrap());
        assert!(other.module_residency_held());
    }

    #[test]
    fn stale_preparation_failure_cannot_release_after_an_open_and_close() {
        let _module = crate::ingress::acquire_test_module_lease();
        let runtime = physical();
        let acquisition = OpenResidency::acquire(&runtime, anchor as *const ()).unwrap();
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let access = runtime.bind_addin_lifecycle().unwrap();
        assert_eq!(
            runtime.remove_addin(
                &access,
                &mut crate::host_callback::HostCallbackSession::new()
            ),
            1
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        acquisition.preparation_failed();
        assert!(runtime.module_residency_held());
    }

    #[test]
    fn a_boundary_cannot_release_another_runtime_or_a_logical_policy() {
        let issuer = physical();
        let target = physical();
        OpenResidency::acquire(&target, anchor as *const ())
            .unwrap()
            .preparation_failed();
        let retained = OpenResidency::acquire(&target, anchor as *const ()).unwrap();
        target.complete_explicit_removal();
        crate::boundary::host::with_test_close_boundary(&issuer, |boundary| {
            assert_eq!(target.release_closed_residency(&boundary).unwrap(), None);
            assert!(target.module_residency_held());
        });
        drop(retained);
        let logical = Runtime::<Probe>::new();
        drop(OpenResidency::acquire(&logical, anchor as *const ()).unwrap());
        logical.complete_explicit_removal();
        crate::boundary::host::with_test_close_boundary(&logical, |boundary| {
            assert_eq!(logical.release_closed_residency(&boundary).unwrap(), None);
            assert!(logical.module_residency_held());
        });
    }

    #[test]
    fn close_release_consumes_current_locked_state_and_rechecks_native_barriers() {
        let runtime = physical();
        drop(OpenResidency::acquire(&runtime, anchor as *const ()).unwrap());
        crate::boundary::host::with_test_close_boundary(&runtime, |boundary| {
            runtime.request_explicit_removal();
            assert_eq!(runtime.release_closed_residency(&boundary).unwrap(), None);
            assert_eq!(
                runtime.host_intent(),
                HostLifecycleIntent::ExplicitRemovalRequested
            );
            runtime.complete_explicit_removal();
            assert_eq!(
                runtime.release_closed_residency(&boundary).unwrap(),
                Some(ResidencyReleaseOutcome::Released)
            );
            assert_eq!(runtime.release_closed_residency(&boundary).unwrap(), None);
            runtime.complete_explicit_removal();
            assert_eq!(
                runtime.release_closed_residency(&boundary).unwrap(),
                Some(ResidencyReleaseOutcome::AlreadyReleased)
            );
        });
    }
}
