//! One advancing transaction retains its registry, old allocation and target.
use super::*;
use crate::sync::MutexGuard;

struct AdvanceOwner<'registry> {
    registry: &'registry TaskRegistry,
    _transition: MutexGuard<'registry, ()>,
    old: CalculationPin,
    next: CalculationEpoch,
}

enum AdvanceAcquisition<'registry> {
    Done(bool),
    Pending(AdvanceOwner<'registry>),
}

impl<'registry> AdvanceOwner<'registry> {
    fn acquire(registry: &'registry TaskRegistry) -> AdvanceAcquisition<'registry> {
        let transition = registry.transition.lock();
        let mut control = registry.control.lock();
        let publication = registry.publication.lock();
        // SAFETY: publication excludes allocation reclamation while pinning.
        let current = unsafe { &*registry.current.load(Ordering::Acquire) };
        let Some(next) = current.epoch.checked_next() else {
            return AdvanceAcquisition::Done(false);
        };
        if let Some(mut stopped) = control.stopped() {
            let state = PublishedOwner::new(CalculationState::new(next));
            state.seal_admission();
            let pointer = NonNull::from(state.as_ref());
            stopped.calculations().insert(next, state);
            registry.publish_current(pointer, next);
            stopped
                .calculations()
                .retain(|id, state| *id == next || state.pin_count() != 0);
            return AdvanceAcquisition::Done(true);
        }
        if registry.closing.load(Ordering::Acquire) {
            return AdvanceAcquisition::Done(false);
        }
        let Some(running) = control.running() else {
            return AdvanceAcquisition::Done(false);
        };
        current.seal_admission();
        // SAFETY: publication retains this exact old allocation while pinning.
        let old = unsafe { CalculationPin::acquire(current) };
        running.begin_advance();
        drop(publication);
        drop(control);
        AdvanceAcquisition::Pending(Self {
            registry,
            _transition: transition,
            old,
            next,
        })
    }

    fn finish(self) -> bool {
        let Self {
            registry,
            _transition,
            old,
            next,
        } = self;
        let _drained_admission = old.get().close_admission().wait();
        let mut control = registry.control.lock();
        // Close may preempt the in-flight advance while admission drains. The
        // transition lock excludes another advance; this owner retains its own
        // old allocation/target instead of reconstructing them from a snapshot.
        let Some(mut advancing) = control.advancing() else {
            return false;
        };
        if registry.closing.load(Ordering::Acquire) {
            advancing.finish();
            return false;
        }
        let publication = registry.publication.lock();
        let state = PublishedOwner::new(CalculationState::new(next));
        let pointer = NonNull::from(state.as_ref());
        advancing.calculations().insert(next, state);
        registry.publish_current(pointer, next);
        drop(old);
        advancing
            .calculations()
            .retain(|id, state| *id == next || state.pin_count() != 0);
        advancing.finish();
        drop(publication);
        drop(control);
        drop(_transition);
        #[cfg(test)]
        {
            let hook = registry.after_publish_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        true
    }
}

impl TaskRegistry {
    pub(crate) fn advance(&self) -> bool {
        #[cfg(test)]
        {
            let hook = self.before_transition_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        match AdvanceOwner::acquire(self) {
            AdvanceAcquisition::Done(result) => result,
            AdvanceAcquisition::Pending(owner) => owner.finish(),
        }
    }
}
