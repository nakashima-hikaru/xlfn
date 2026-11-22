//! Locked phase acquisition and issuer-bound registry close ownership.
use super::*;
use crate::sync::MutexGuard;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use xlfn_kernel::published_owner::PublishedOwner;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

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

type RetiredCalculations = SmallVec<[PublishedOwner<CalculationState>; 1]>;

fn take_retired_calculations(
    calculations: &mut FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>>,
    current: CalculationEpoch,
) -> RetiredCalculations {
    // After publication, new snapshots can pin only the successor. Holding
    // control still excludes other acquisitions of retired allocations;
    // an existing pin can only release its reservation. Thus an Acquire
    // zero observation is terminal for any non-current owner.
    calculations
        .extract_if(|id, state| *id != current && state.pin_count() == 0)
        .map(|(_, state)| state)
        .collect()
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
    pub(super) fn take_retired(&mut self, current: CalculationEpoch) -> RetiredCalculations {
        take_retired_calculations(&mut self.0.calculations, current)
    }

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
    pub(super) fn take_retired(&mut self, current: CalculationEpoch) -> RetiredCalculations {
        take_retired_calculations(&mut self.0.calculations, current)
    }

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
    pending: RollbackGuard<PendingClose<'registry>>,
}

struct PendingClose<'registry> {
    registry: &'registry TaskRegistry,
}

impl Rollback for PendingClose<'_> {
    fn rollback(self) {
        let mut control = self.registry.control.lock();
        if matches!(control.state, State::CloseClaimed) {
            control.state = State::CloseAvailable;
        }
    }
}

impl<'registry> CloseClaim<'registry> {
    pub(super) fn acquire(
        registry: &'registry TaskRegistry,
        prepare_locked: impl FnOnce(&RegistryControl) -> Vec<CalculationPin>,
    ) -> XllResult<(Self, Vec<CalculationPin>)> {
        // Release the mutex before rollback on unwind. The lock never travels
        // with the issued claim.
        let claim;
        let mut control = registry.control.lock();
        if matches!(control.state, State::CloseClaimed) {
            return Err(XllError::Closing);
        }
        control.state = State::CloseClaimed;
        claim = Self {
            pending: RollbackGuard::new(PendingClose { registry }),
        };
        let calculations = prepare_locked(&control);
        drop(control);
        Ok((claim, calculations))
    }
    pub(super) fn registry(&self) -> &'registry TaskRegistry {
        self.pending.get().registry
    }
    pub(super) fn finish(self) {
        let mut control = self.registry().control.lock();
        if !matches!(control.state, State::CloseClaimed) {
            xlfn_kernel::invariant::fail_stop();
        }
        self.pending.complete();
        control.state = State::Stopped;
    }
}

#[cfg(test)]
#[test]
fn miri_close_preparation_unwind_releases_the_lock_before_returning_the_claim() {
    let registry = TaskRegistry::new();
    let error = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = CloseClaim::acquire(&registry, |_| panic!("injected close preparation panic"));
    }));
    assert!(error.is_err());
    assert!(matches!(
        registry.control.lock().state,
        State::CloseAvailable
    ));
    registry.request_close().unwrap().cancel().wait().finish();
    assert!(matches!(registry.control.lock().state, State::Stopped));
}

/// Preparation retains the actual canonical/publication-management guards and
/// an unpublished candidate. Abandonment changes no registry state. The
/// candidate drops after the guards, so no payload destruction runs under them.
pub(in crate::async_udf) struct RegistryPreparedStart<'registry> {
    publication: MutexGuard<'registry, ()>,
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
            publication,
            mut control,
            transition,
            activity,
            registry,
            candidate,
            limit,
        } = self;
        let epoch = candidate.epoch;
        let pointer = NonNull::from(candidate.as_ref());
        // The same publication guard covers the zero-pin check and the
        // replacement. A concurrent snapshot can only pin the new allocation.
        let previous = control.calculations.insert(epoch, candidate);
        registry.publish_current(pointer, epoch);
        registry.limit.store(limit.get(), Ordering::Relaxed);
        registry.closing.store(false, Ordering::Release);
        control.state = State::Running;
        drop(activity.reopen());
        drop(publication);
        let retired = take_retired_calculations(&mut control.calculations, epoch);
        drop(control);
        drop(transition);
        drop(previous);
        drop(retired);
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
        // Control excludes a concurrent change of the published epoch, but
        // snapshots need only publication and can proceed during allocation.
        let candidate = PublishedOwner::new(CalculationState::new(self.current_epoch()));
        let publication = self.publication.lock();
        // SAFETY: publication retains the exact old allocation through this read.
        let current = unsafe { &*self.current.load(Ordering::Acquire) };
        if current.pin_count() != 0 {
            drop(publication);
            drop(control);
            drop(transition);
            drop(candidate);
            return Err(XllError::Closing);
        }
        let activity = self.activity.control().seal().wait();
        Ok(RegistryPreparedStart {
            publication,
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
    fn miri_prepared_registry_start_excludes_old_snapshots_until_publication() {
        let registry = TaskRegistry::new();
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        registry.request_close().unwrap().cancel().wait().finish();
        let old = registry.current.load(Ordering::Acquire);
        let prepared = registry
            .prepare_start(crate::AsyncTaskLimit::DEFAULT)
            .unwrap();
        assert!(registry.publication.try_lock().is_none());

        std::thread::scope(|scope| {
            let (attempted, received) = std::sync::mpsc::channel();
            let registry = &registry;
            let reader = scope.spawn(move || {
                attempted.send(()).unwrap();
                registry.snapshot()
            });
            received.recv().unwrap();
            prepared.publish();
            let snapshot = reader.join().unwrap();
            assert_eq!(snapshot.epoch(), CalculationEpoch::INITIAL);
            assert_ne!(snapshot.calculation.pointer().as_ptr(), old);
        });
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
