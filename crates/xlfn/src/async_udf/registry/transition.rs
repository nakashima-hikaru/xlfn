//! One advancing transaction retains its registry, old allocation and target.
use super::*;
use crate::sync::MutexGuard;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

struct AdvanceOwner<'registry> {
    pending: RollbackGuard<PendingAdvance<'registry>>,
    old: CalculationPin,
    next: CalculationEpoch,
}

struct PendingAdvance<'registry> {
    registry: &'registry TaskRegistry,
    _transition: MutexGuard<'registry, ()>,
}

impl Rollback for PendingAdvance<'_> {
    fn rollback(self) {
        // The old admission may already be sealed. Leave the canonical phase
        // and installed calculations for close to claim and drain. This must
        // also work while unwinding with either registry mutex still held.
        self.registry.close_admission();
    }
}

enum AdvanceAcquisition<'registry> {
    Done(bool),
    Pending(AdvanceOwner<'registry>),
}

impl<'registry> AdvanceOwner<'registry> {
    fn allocate_successor(
        #[cfg(test)] registry: &TaskRegistry,
        next: CalculationEpoch,
    ) -> PublishedOwner<CalculationState> {
        #[cfg(test)]
        tests::before_successor_allocation(registry);
        PublishedOwner::new(CalculationState::new(next))
    }

    fn acquire(registry: &'registry TaskRegistry) -> AdvanceAcquisition<'registry> {
        // Declare the possible displaced owner before locks so unwinding
        // also releases the management guards before its destruction.
        let displaced;
        let transition = registry.transition.lock();
        let mut control = registry.control.lock();
        // Control excludes changes to the current generation (transition
        // also serializes advances); only pinning requires publication.
        let Some(next) = registry.current_epoch().checked_next() else {
            return AdvanceAcquisition::Done(false);
        };
        if let Some(mut stopped) = control.stopped() {
            let state = Self::allocate_successor(
                #[cfg(test)]
                registry,
                next,
            );
            state.seal_admission();
            let pointer = NonNull::from(state.as_ref());
            // Insertion under control cannot affect concurrent snapshots
            // before the atomic current pointer changes.
            // A previously interrupted advance may have inserted an owner
            // at this epoch without publishing it.
            displaced = stopped.calculations().insert(next, state);
            {
                let _publication = registry.publication.lock();
                registry.publish_current(pointer, next);
            }
            let retired = stopped.take_retired(next);
            drop(control);
            drop(transition);
            drop(displaced);
            drop(retired);
            return AdvanceAcquisition::Done(true);
        }
        if registry.closing.load(Ordering::Acquire) {
            return AdvanceAcquisition::Done(false);
        }
        let Some(running) = control.running() else {
            return AdvanceAcquisition::Done(false);
        };
        let publication = registry.publication.lock();
        // SAFETY: publication excludes allocation reclamation while pinning.
        let current = unsafe { &*registry.current.load(Ordering::Acquire) };
        let pending = RollbackGuard::new(PendingAdvance {
            registry,
            _transition: transition,
        });
        current.seal_admission();
        #[cfg(test)]
        tests::inject_fault(tests::AdvanceFault::Sealed);
        // SAFETY: publication retains this exact old allocation while pinning.
        let old = unsafe { CalculationPin::acquire(current) };
        running.begin_advance();
        drop(publication);
        drop(control);
        AdvanceAcquisition::Pending(Self { pending, old, next })
    }

    fn finish(self) -> bool {
        let registry = self.pending.get().registry;
        let _drained_admission = self.old.get().close_admission().wait();
        let displaced;
        let mut control = registry.control.lock();
        // Close may preempt the in-flight advance while admission drains. The
        // transition lock excludes another advance; this owner retains its own
        // old allocation/target instead of reconstructing them from a snapshot.
        let mut advancing = match control.advancing() {
            Some(advancing) if !registry.closing.load(Ordering::Acquire) => advancing,
            advancing => {
                if let Some(advancing) = advancing {
                    advancing.finish();
                }
                drop(control);
                let Self { pending, old, .. } = self;
                drop(old);
                drop(pending.complete());
                return false;
            }
        };
        let state = Self::allocate_successor(
            #[cfg(test)]
            registry,
            self.next,
        );
        let pointer = NonNull::from(state.as_ref());
        displaced = advancing.calculations().insert(self.next, state);
        let publication = registry.publication.lock();
        // After publication the successor's gate is open. On unwind, publish
        // close intent before unlocking publication and exposing the successor
        // to new snapshots.
        let Self { pending, old, next } = self;
        #[cfg(test)]
        tests::inject_fault(tests::AdvanceFault::BeforePublication);
        registry.publish_current(pointer, next);
        #[cfg(test)]
        tests::inject_fault(tests::AdvanceFault::Published);
        let pending = pending.complete();
        drop(publication);
        drop(old);
        let retired = advancing.take_retired(next);
        advancing.finish();
        drop(control);
        drop(pending);
        drop(displaced);
        drop(retired);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancellation::{CancellationGuarantee, CancellationSource};
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum AdvanceFault {
        Sealed,
        BeforePublication,
        Published,
    }

    std::thread_local! {
        static ADVANCE_FAULT: Cell<Option<AdvanceFault>> = const { Cell::new(None) };
    }

    pub(super) fn inject_fault(point: AdvanceFault) {
        if ADVANCE_FAULT.get() == Some(point) {
            ADVANCE_FAULT.set(None);
            panic!("injected calculation advance fault at {point:?}");
        }
    }

    std::thread_local! {
        static INSPECT_SUCCESSOR_ALLOCATION: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn before_successor_allocation(registry: &TaskRegistry) {
        if INSPECT_SUCCESSOR_ALLOCATION.replace(false) {
            assert!(
                registry.publication.try_lock().is_some(),
                "a launch snapshot must not wait for successor allocation"
            );
            // Still pin the previous generation through the normal reader path.
            assert_eq!(registry.snapshot().epoch(), registry.current_epoch());
        }
    }

    #[test]
    fn miri_advance_allocates_stopped_and_running_successors_without_publication_lock() {
        let registry = TaskRegistry::new();
        INSPECT_SUCCESSOR_ALLOCATION.set(true);
        assert!(registry.advance());
        assert!(!INSPECT_SUCCESSOR_ALLOCATION.get());
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        INSPECT_SUCCESSOR_ALLOCATION.set(true);
        assert!(registry.advance());
        assert!(!INSPECT_SUCCESSOR_ALLOCATION.get());
        assert_eq!(registry.control.lock().calculations().len(), 1);
        registry.request_close().unwrap().cancel().wait().finish();
    }

    #[test]
    fn miri_stopped_advance_replaces_an_unpublished_successor_after_close() {
        let registry = TaskRegistry::new();
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        let old = registry.current_epoch();
        let next = old.checked_next().unwrap();
        ADVANCE_FAULT.set(Some(AdvanceFault::BeforePublication));
        let error = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            registry.advance();
        }));
        assert!(error.is_err());
        assert_eq!(registry.current_epoch(), old);
        let unpublished = registry.control.lock().calculations()[&next].as_ptr();

        registry.request_close().unwrap().cancel().wait().finish();
        assert!(registry.advance());
        assert_eq!(registry.current_epoch(), next);
        assert_ne!(registry.current.load(Ordering::Acquire), unpublished);
        assert_eq!(registry.control.lock().calculations().len(), 1);
    }

    #[test]
    fn miri_advance_unwind_preserves_canonical_close_and_restart() {
        use AdvanceFault::*;
        for fault in [Sealed, BeforePublication, Published] {
            let registry = TaskRegistry::new();
            registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
            let old_snapshot = registry.snapshot();
            let old = old_snapshot.epoch();
            let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
            let task = registry
                .reserve(old)
                .unwrap()
                .commit(std::future::pending::<()>(), source);

            ADVANCE_FAULT.set(Some(fault));
            let error =
                crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                    registry.advance();
                }));
            assert!(error.is_err());
            assert_eq!(ADVANCE_FAULT.get(), None);

            let expected = if fault == Published {
                old.checked_next().unwrap()
            } else {
                old
            };
            assert_eq!(registry.current_epoch(), expected);
            assert_eq!(registry.snapshot().epoch(), expected);
            assert!(matches!(
                registry.preflight_available(),
                Err(__err) if __err.is_closing()));
            assert!(matches!(old_snapshot.reserve(), Err(__err) if __err.is_closing()));
            assert!(matches!(
                registry.snapshot().reserve(),
                Err(__err) if __err.is_closing()));
            assert!(!registry.advance());
            assert!(matches!(
                registry.prepare_start(crate::AsyncTaskLimit::DEFAULT),
                Err(__err) if __err.is_closing()));
            assert!(!token.is_cancelled());

            let closing = registry.request_close().unwrap().cancel();
            assert!(token.is_cancelled());
            drop(task);
            closing.wait().finish();

            registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
            assert_eq!(registry.snapshot().epoch(), expected);
            drop(registry.reserve(expected).unwrap());
            assert!(registry.advance());
            assert_eq!(registry.current_epoch(), expected.checked_next().unwrap());
            registry.request_close().unwrap().cancel().wait().finish();
        }
    }

    #[test]
    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[cfg_attr(miri, ignore)]
    fn loom_interrupted_publication_precedes_successor_snapshot() {
        use loom::sync::atomic::AtomicBool as LoomAtomicBool;
        use loom::sync::{Arc, Mutex};

        struct CloseOnDrop<'a>(&'a LoomAtomicBool);

        impl Rollback for CloseOnDrop<'_> {
            fn rollback(self) {
                self.0.store(true, Ordering::Release);
            }
        }

        loom::model(|| {
            let state = Arc::new((Mutex::new(false), LoomAtomicBool::new(false)));
            let writer = Arc::clone(&state);
            let advance = loom::thread::spawn(move || {
                let mut successor = writer.0.lock().unwrap();
                let _pending = RollbackGuard::new(CloseOnDrop(&writer.1));
                *successor = true;
                // Abandonment stores close intent before releasing publication.
            });
            let successor = *state.0.lock().unwrap();
            if successor {
                assert!(state.1.load(Ordering::Acquire));
            }
            advance.join().unwrap();
        });
    }
}
