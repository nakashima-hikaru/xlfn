//! Queue-bound retirement capabilities issued by the exact transition.
use super::*;
use std::mem::ManuallyDrop;

/// A transfer in progress while a transition lock may still be held. Its field
/// never runs payload destructors, even if panic or abandonment drops this
/// wrapper. Only the completed outer operation can make it disposable.
struct DetachedLocked<'owner, const N: usize, Q: RetirementStorage> {
    owner: &'owner RotatingRetirementDomain<N, Q>,
    records: ManuallyDrop<Q>,
}

impl<'owner, const N: usize, Q: RetirementStorage> DetachedLocked<'owner, N, Q> {
    fn detach(owner: &'owner RotatingRetirementDomain<N, Q>, mut queue: MutexGuard<'_, Q>) -> Self {
        Self {
            owner,
            records: ManuallyDrop::new(super::queue::take_retired!(&mut *queue)),
        }
    }

    fn after_unlock(self) -> DisposableBatch<'owner, N, Q> {
        DisposableBatch {
            owner: self.owner,
            records: ManuallyDrop::into_inner(self.records),
        }
    }
}

/// Owners whose readers have drained and whose kernel queue and transition
/// locks have both been released. This does not certify caller-held locks.
#[must_use = "the batch retains its payload until consumed or dropped"]
pub struct DisposableBatch<'owner, const N: usize, Q: RetirementStorage> {
    owner: &'owner RotatingRetirementDomain<N, Q>,
    records: Q,
}

impl<const N: usize, Q: RetirementStorage> DisposableBatch<'_, N, Q> {
    pub fn belongs_to(&self, owner: &RotatingRetirementDomain<N, Q>) -> bool {
        std::ptr::eq(self.owner, owner)
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn into_inner(self) -> Q {
        self.records
    }
}

/// Repeatable observation of permanent closure, bound to the issuing queues.
pub struct ClosedRetirement<'owner, const N: usize, Q: RetirementStorage> {
    owner: &'owner RotatingRetirementDomain<N, Q>,
    _certificate: ClosedDomain<'owner>,
}

impl<const N: usize, Q: RetirementStorage> ClosedRetirement<'_, N, Q> {
    pub fn belongs_to(&self, owner: &RotatingRetirementDomain<N, Q>) -> bool {
        std::ptr::eq(self.owner, owner)
    }

    pub fn take_queues(&self) -> [DisposableBatch<'_, N, Q>; 2] {
        // This terminal receiver retains its issuing domain; no caller can
        // pair a certificate with another queue array.
        let first = self.owner.queues[0].lock();
        let second = self.owner.queues[1].lock();
        let batches = [
            DetachedLocked::detach(self.owner, first),
            DetachedLocked::detach(self.owner, second),
        ];
        // take_queues released both queue guards; terminal closure released
        // its transition guard before this observation was returned.
        batches.map(DetachedLocked::after_unlock)
    }
}

impl<const N: usize, Q: RetirementStorage> RotatingRetirementDomain<N, Q> {
    /// Drains and transfers retired work. No caller can obtain a disposable
    /// owner until the entire transition (including pending retirement) ends.
    pub fn quiesce(&self) -> Result<[Option<DisposableBatch<'_, N, Q>>; 2], DomainClosed> {
        let batches = self.domain.quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].lock(),
            |certificate| {
                // The scoped certificate was just issued by self.domain.
                // Its index selects self's queue inside this issuing operation;
                // there is no independent domain/certificate join API.
                DetachedLocked::detach(self, self.queues[certificate.index.index()].lock())
            },
        )?;
        Ok(batches.map(|batch| batch.map(|batch| batch.after_unlock())))
    }

    pub fn try_quiesce_if_idle(&self) -> Option<Result<DisposableBatch<'_, N, Q>, DomainClosed>> {
        let batch = self.domain.try_quiesce_if_idle_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                // The scoped certificate was just issued by self.domain.
                // Its index selects self's queue inside this issuing operation;
                // there is no independent domain/certificate join API.
                DetachedLocked::detach(self, self.queues[certificate.index.index()].lock())
            },
        )?;
        Some(batch.map(|batch| batch.after_unlock()))
    }

    pub fn poll_quiesce(&self) -> Option<Result<DisposableBatch<'_, N, Q>, DomainClosed>> {
        let batch = self.domain.poll_quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                // The scoped certificate was just issued by self.domain.
                // Its index selects self's queue inside this issuing operation;
                // there is no independent domain/certificate join API.
                DetachedLocked::detach(self, self.queues[certificate.index.index()].lock())
            },
        )?;
        Some(batch.map(|batch| batch.after_unlock()))
    }

    pub fn seal_and_wait(&self) -> ClosedRetirement<'_, N, Q> {
        ClosedRetirement {
            owner: self,
            _certificate: self.domain.seal_and_wait(),
        }
    }
}

impl<const N: usize, Q: RetirementStorage> Drop for RotatingRetirementDomain<N, Q> {
    fn drop(&mut self) {
        // The detached-permit owner contract must end before exclusive owner
        // destruction starts. Both guards end before queue field destructors.
        let _closed = self.domain.seal_and_wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize};

    struct Probe(Arc<AtomicUsize>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn miri_registration_capacity_panic_retains_incoming_owners() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct ZeroSizedOwner;
        impl Drop for ZeroSizedOwner {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        for bulk in [false, true] {
            // A ZST vector can represent this many valid elements without an
            // allocation. Suppress normal Drop of the synthetic huge length.
            let mut full = ManuallyDrop::new(Vec::<ZeroSizedOwner>::new());
            // SAFETY: the element has no fields, so every ZST slot is valid.
            unsafe {
                full.set_len(usize::MAX);
            }
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if bulk {
                    full.append_owned(vec![ZeroSizedOwner]);
                } else {
                    full.append_one(ZeroSizedOwner);
                }
            }));
            assert!(failure.is_err());
            assert_eq!(DROPS.load(Ordering::Relaxed), 0);
            // SAFETY: forget only the synthetic ZST owners; no heap allocation
            // is involved. The failed incoming owner is intentionally retained.
            unsafe {
                full.set_len(0);
            }
            drop(ManuallyDrop::into_inner(full));
        }
    }

    #[test]
    fn miri_registration_panic_retains_all_registered_owners() {
        let drops = Arc::new(AtomicUsize::new(0));
        let domain = RotatingRetirementDomain::<2, Vec<Probe>>::new();
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            domain.register_retired(|_, mut registration| {
                registration.append(Probe(Arc::clone(&drops)));
                panic!("after append");
            });
        }));
        assert!(failure.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(domain.seal_and_wait().take_queues());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[allow(
        clippy::drop_non_drop,
        reason = "dropping a locked transfer must suppress payload destruction"
    )]
    fn miri_abandoned_locked_transfer_does_not_destroy_payload() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = RotatingRetirementDomain::<1, Vec<Probe>>::new();
        let queue = Mutex::new(vec![Probe(Arc::clone(&drops))]);
        let batch = DetachedLocked::detach(&owner, queue.lock());
        // SAFETY: preserve the suppressed-drop storage only for later cleanup.
        // The original wrapper cannot access or destroy it after being dropped.
        let records = unsafe { std::ptr::read(&batch.records) };
        drop(batch);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(ManuallyDrop::into_inner(records));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        clippy::forget_non_drop,
        reason = "forgotten transfer retains payload until explicit test-only cleanup"
    )]
    fn miri_forgotten_locked_transfer_does_not_destroy_payload() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = RotatingRetirementDomain::<1, Vec<Probe>>::new();
        let queue = Mutex::new(vec![Probe(Arc::clone(&drops))]);
        let batch = DetachedLocked::detach(&owner, queue.lock());
        // SAFETY: the forgotten wrapper cannot access or destroy these owners;
        // retain its suppressed-drop bits solely for leak-free test cleanup.
        let records = unsafe { std::ptr::read(&batch.records) };
        std::mem::forget(batch);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(ManuallyDrop::into_inner(records));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_disposable_payload_can_reenter_queue_and_transition() {
        use std::sync::Weak;
        struct Reenter {
            owner: Weak<RotatingRetirementDomain<1, Vec<Reenter>>>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                let owner = self.owner.upgrade().unwrap();
                assert!(owner.try_queue_lens().is_some());
                assert!(owner.domain.transition.try_lock().is_some());
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        let owner = Arc::new(RotatingRetirementDomain::<1, Vec<Reenter>>::new());
        let drops = Arc::new(AtomicUsize::new(0));
        owner.register_retired(|_, mut registration| {
            registration.append(Reenter {
                owner: Arc::downgrade(&owner),
                drops: Arc::clone(&drops),
            })
        });
        drop(owner.quiesce().unwrap());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_small_retirement_batches_keep_inline_storage() {
        let domain = RotatingRetirementDomain::<1, smallvec::SmallVec<[usize; 4]>>::new();
        domain.register_retired(|_, mut registration| {
            registration.append_batch(smallvec::smallvec![1, 2, 3])
        });
        let batch = domain
            .quiesce()
            .unwrap()
            .into_iter()
            .flatten()
            .next()
            .unwrap()
            .into_inner();
        assert!(!batch.spilled());
        assert_eq!(batch.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn miri_retirement_owner_drop_destroys_queued_payload_after_terminal_drain() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = RotatingRetirementDomain::<2, Vec<Probe>>::new();
        let reader = owner.enter(0).unwrap();
        owner
            .register_retired(|_, mut registration| registration.append(Probe(Arc::clone(&drops))));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(reader);
        let closed = owner.seal_and_wait();
        assert!(closed.belongs_to(&owner));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
