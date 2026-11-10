//! Locked phase acquisition and issuer-bound registry close ownership.
use super::*;
use crate::sync::MutexGuard;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use xlfn_kernel::published_owner::PublishedOwner;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::async_udf) enum ControlPhase {
    Stopped,
    Running,
    Advancing {},
    Closing,
}

enum State {
    Stopped,
    Running,
    Advancing {},
    CloseAvailable,
    // Only the issued CloseClaim can return or complete this owned state.
    CloseClaimed,
}

pub(in crate::async_udf) struct RegistryControl {
    state: State,
    pub(super) calculations: FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>>,
}

pub(super) struct StoppedControl<'guard>(&'guard mut RegistryControl);
pub(super) struct RunningControl<'guard>(&'guard mut RegistryControl);
pub(super) struct AdvancingControl<'guard>(&'guard mut RegistryControl);

impl RegistryControl {
    pub(super) fn new(initial: PublishedOwner<CalculationState>) -> Self {
        Self {
            state: State::Stopped,
            calculations: FxHashMap::from_iter([(initial.epoch, initial)]),
        }
    }
    #[cfg(test)]
    pub(in crate::async_udf) fn phase(&self) -> ControlPhase {
        match self.state {
            State::Stopped => ControlPhase::Stopped,
            State::Running => ControlPhase::Running,
            State::Advancing {} => ControlPhase::Advancing {},
            State::CloseAvailable | State::CloseClaimed => ControlPhase::Closing,
        }
    }
    #[cfg(test)]
    pub(in crate::async_udf) fn calculations(
        &self,
    ) -> &FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>> {
        &self.calculations
    }

    pub(super) fn stopped(&mut self) -> Option<StoppedControl<'_>> {
        matches!(self.state, State::Stopped).then_some(StoppedControl(self))
    }
    pub(super) fn running(&mut self) -> Option<RunningControl<'_>> {
        matches!(self.state, State::Running).then_some(RunningControl(self))
    }
    pub(super) fn advancing(&mut self) -> Option<AdvancingControl<'_>> {
        matches!(self.state, State::Advancing { .. }).then_some(AdvancingControl(self))
    }
}
impl StoppedControl<'_> {
    pub(super) fn calculations(
        &mut self,
    ) -> &mut FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>> {
        &mut self.0.calculations
    }
}
impl RunningControl<'_> {
    pub(super) fn begin_advance(self) {
        self.0.state = State::Advancing {};
    }
}
impl AdvancingControl<'_> {
    pub(super) fn calculations(
        &mut self,
    ) -> &mut FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>> {
        &mut self.0.calculations
    }
    pub(super) fn finish(self) {
        self.0.state = State::Running;
    }
}

/// The target and completion responsibility never travel as separate arguments.
/// Canonical CloseClaimed storage cannot be acquired by start/advance or another
/// close. Forgetting the claim therefore leaves the registry unavailable.
pub(super) struct CloseClaim<'registry> {
    registry: &'registry TaskRegistry,
    pending: bool,
}
impl<'registry> CloseClaim<'registry> {
    pub(super) fn acquire(
        registry: &'registry TaskRegistry,
    ) -> XllResult<(Self, MutexGuard<'registry, RegistryControl>)> {
        let mut control = registry.control.lock();
        if matches!(control.state, State::CloseClaimed) {
            return Err(XllError::Closing);
        }
        control.state = State::CloseClaimed;
        Ok((
            Self {
                registry,
                pending: true,
            },
            control,
        ))
    }
    pub(super) fn registry(&self) -> &'registry TaskRegistry {
        self.registry
    }
    pub(super) fn finish(mut self) {
        self.registry.control.lock().state = State::Stopped;
        self.pending = false;
    }
}
impl Drop for CloseClaim<'_> {
    fn drop(&mut self) {
        if self.pending {
            self.registry.control.lock().state = State::CloseAvailable;
        }
    }
}

/// Preparation retains the actual canonical/publication-management guards and
/// an unpublished candidate. Abandonment changes no registry state. The
/// candidate drops after the guards, so no payload destruction runs under them.
pub(in crate::async_udf) struct RegistryPreparedStart<'registry> {
    control: MutexGuard<'registry, RegistryControl>,
    transition: MutexGuard<'registry, ()>,
    activity: xlfn_kernel::drain_gate::GateControl<'registry, xlfn_kernel::drain_gate::GateDrained>,
    registry: &'registry TaskRegistry,
    candidate: PublishedOwner<CalculationState>,
    limit: crate::addin_impl::AsyncTaskLimit,
}

impl RegistryPreparedStart<'_> {
    pub(in crate::async_udf) fn publish(self) {
        let Self {
            mut control,
            transition,
            activity,
            registry,
            candidate,
            limit,
        } = self;
        let publication = registry.publication.lock();
        let epoch = candidate.epoch;
        let pointer = NonNull::from(candidate.as_ref());
        control.calculations.insert(epoch, candidate);
        control
            .calculations
            .retain(|id, state| *id == epoch || state.pin_count() != 0);
        registry.publish_current(pointer, epoch);
        registry.limit.store(limit.get(), Ordering::Relaxed);
        registry.closing.store(false, Ordering::Release);
        control.state = State::Running;
        drop(activity.reopen());
        drop(publication);
        drop(control);
        drop(transition);
    }
}

impl TaskRegistry {
    pub(in crate::async_udf) fn prepare_start(
        &self,
        limit: crate::addin_impl::AsyncTaskLimit,
    ) -> XllResult<RegistryPreparedStart<'_>> {
        let transition = self.transition.lock();
        let control = self.control.lock();
        if !matches!(control.state, State::Stopped) {
            return Err(XllError::Closing);
        }
        let publication = self.publication.lock();
        // SAFETY: publication retains the exact old allocation through this read.
        let current = unsafe { &*self.current.load(Ordering::Acquire) };
        if current.pin_count() != 0 {
            return Err(XllError::Closing);
        }
        let candidate = PublishedOwner::new(CalculationState::new(current.epoch));
        let activity = self.activity.control().seal().wait();
        drop(publication);
        Ok(RegistryPreparedStart {
            control,
            transition,
            activity,
            registry: self,
            candidate,
            limit,
        })
    }

    #[cfg(test)]
    pub(crate) fn start(&self, limit: crate::addin_impl::AsyncTaskLimit) -> XllResult<()> {
        self.prepare_start(limit)?.publish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_prepared_registry_start_abandonment_preserves_canonical_publication() {
        let registry = TaskRegistry::new();
        let original = registry.current.load(Ordering::Acquire);
        let prepared = registry
            .prepare_start(crate::AsyncTaskLimit::DEFAULT)
            .unwrap();
        assert_eq!(registry.current.load(Ordering::Acquire), original);
        assert!(registry.control.try_lock().is_none());
        drop(prepared);
        assert!(matches!(
            registry.control.lock().phase(),
            ControlPhase::Stopped
        ));
        assert_eq!(registry.current.load(Ordering::Acquire), original);
        assert!(matches!(
            registry.preflight_available(),
            Err(XllError::Closing)
        ));
        registry
            .prepare_start(crate::AsyncTaskLimit::DEFAULT)
            .unwrap()
            .publish();
        assert!(registry.preflight_available().unwrap());
        registry.request_close().unwrap().cancel().wait().finish();
    }

    #[test]
    fn miri_registry_close_claim_returns_for_retry_without_reopening() {
        let registry = TaskRegistry::new();
        registry
            .prepare_start(crate::AsyncTaskLimit::DEFAULT)
            .unwrap()
            .publish();
        let closing = registry.request_close().unwrap();
        assert!(registry.request_close().is_err());
        assert!(
            registry
                .prepare_start(crate::AsyncTaskLimit::DEFAULT)
                .is_err()
        );
        drop(closing);
        assert!(matches!(
            registry.preflight_available(),
            Err(XllError::Closing)
        ));
        registry.request_close().unwrap().cancel().wait().finish();
        registry
            .prepare_start(crate::AsyncTaskLimit::DEFAULT)
            .unwrap()
            .publish();
        registry.request_close().unwrap().cancel().wait().finish();
    }
}
