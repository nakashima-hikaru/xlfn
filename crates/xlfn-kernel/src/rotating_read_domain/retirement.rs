//! Append-only registration and lock-free disposal of generation-bound owners.
use super::*;
use std::mem::ManuallyDrop;

mod storage {
    pub trait Sealed {}
    impl<T> Sealed for Vec<T> {}
    impl<A: smallvec::Array> Sealed for smallvec::SmallVec<A> {}
}

/// Audited queue storage. Only the built-in vector implementations can satisfy
/// the private supertrait: arbitrary interior-mutable or custom-drop queues
/// cannot expose registered owners through observation or disposal callbacks.
#[allow(private_bounds, reason = "retirement storage is deliberately sealed")]
pub trait RetirementStorage: storage::Sealed + Default {
    type Item;
    #[doc(hidden)]
    fn len(&self) -> usize;
    #[doc(hidden)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[doc(hidden)]
    fn append_one(&mut self, owner: Self::Item);
    #[doc(hidden)]
    fn append_owned(&mut self, batch: Self);
}

impl<T> RetirementStorage for Vec<T> {
    type Item = T;
    fn len(&self) -> usize {
        self.len()
    }
    fn append_one(&mut self, owner: T) {
        let owner = ManuallyDrop::new(owner);
        self.reserve(1);
        // Successful reservation leaves no fallible allocation before transfer.
        super::queue::append_retired!(self, ManuallyDrop::into_inner(owner));
    }
    fn append_owned(&mut self, batch: Self) {
        let batch = ManuallyDrop::new(batch);
        self.reserve(batch.len());
        // Audited owning iteration invokes no user code, and every push fits.
        for owner in ManuallyDrop::into_inner(batch) {
            super::queue::append_retired!(self, owner);
        }
    }
}

impl<A: smallvec::Array> RetirementStorage for smallvec::SmallVec<A> {
    type Item = A::Item;
    fn len(&self) -> usize {
        self.len()
    }
    fn append_one(&mut self, owner: A::Item) {
        let owner = ManuallyDrop::new(owner);
        self.reserve(1);
        super::queue::append_retired!(self, ManuallyDrop::into_inner(owner));
    }
    fn append_owned(&mut self, batch: Self) {
        let batch = ManuallyDrop::new(batch);
        self.reserve(batch.len());
        for owner in ManuallyDrop::into_inner(batch) {
            super::queue::append_retired!(self, owner);
        }
    }
}

/// Registration authority exposes no queue reference or removal operation.
pub struct RetirementRegistration<'queue, Q: RetirementStorage> {
    queue: MutexGuard<'queue, Q>,
}

impl<'queue, Q: RetirementStorage> RetirementRegistration<'queue, Q> {
    pub(super) fn new(queue: MutexGuard<'queue, Q>) -> Self {
        Self { queue }
    }

    pub fn append(&mut self, owner: Q::Item) {
        self.queue.append_one(owner);
    }
    pub fn append_batch(&mut self, batch: Q) {
        self.queue.append_owned(batch);
    }
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A transfer in progress while a transition lock may still be held. Its field
/// never runs payload destructors, even if panic or abandonment drops this
/// wrapper. Only the completed outer operation can make it disposable.
struct DetachedLocked<Q: RetirementStorage> {
    records: ManuallyDrop<Q>,
}

impl<Q: RetirementStorage> DetachedLocked<Q> {
    fn detach(mut queue: MutexGuard<'_, Q>) -> Self {
        Self {
            records: ManuallyDrop::new(super::queue::take_retired!(&mut *queue)),
        }
    }

    fn after_unlock<const N: usize>(
        self,
        owner: &RotatingRetirementDomain<N, Q>,
    ) -> DisposableBatch<'_, N, Q> {
        DisposableBatch {
            owner,
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
    certificate: ClosedDomain<'owner>,
}

impl<const N: usize, Q: RetirementStorage> ClosedRetirement<'_, N, Q> {
    pub fn belongs_to(&self, owner: &RotatingRetirementDomain<N, Q>) -> bool {
        std::ptr::eq(self.owner, owner)
    }

    pub fn take_queues(&self) -> [DisposableBatch<'_, N, Q>; 2] {
        let batches = self
            .certificate
            .take_queues(&self.owner.domain, &self.owner.queues, |first, second| {
                [
                    DetachedLocked::detach(first),
                    DetachedLocked::detach(second),
                ]
            })
            .unwrap_or_else(|| crate::invariant::fail_stop());
        // take_queues released both queue guards; terminal closure released
        // its transition guard before this observation was returned.
        batches.map(|batch| batch.after_unlock(self.owner))
    }
}

impl<const N: usize, Q: RetirementStorage> RotatingRetirementDomain<N, Q> {
    /// Drains and transfers retired work. No caller can obtain a disposable
    /// owner until the entire transition (including pending retirement) ends.
    pub fn quiesce(&self) -> Result<[Option<DisposableBatch<'_, N, Q>>; 2], DomainClosed> {
        let batches = self.domain.quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].lock(),
            |certificate| {
                certificate
                    .take_queue(&self.domain, &self.queues, DetachedLocked::detach)
                    .unwrap_or_else(|| crate::invariant::fail_stop())
            },
        )?;
        Ok(batches.map(|batch| batch.map(|batch| batch.after_unlock(self))))
    }

    pub fn try_quiesce_if_idle(&self) -> Option<Result<DisposableBatch<'_, N, Q>, DomainClosed>> {
        let batch = self.domain.try_quiesce_if_idle_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                certificate
                    .take_queue(&self.domain, &self.queues, DetachedLocked::detach)
                    .unwrap_or_else(|| crate::invariant::fail_stop())
            },
        )?;
        Some(batch.map(|batch| batch.after_unlock(self)))
    }

    pub fn poll_quiesce(&self) -> Option<Result<DisposableBatch<'_, N, Q>, DomainClosed>> {
        let batch = self.domain.poll_quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                certificate
                    .take_queue(&self.domain, &self.queues, DetachedLocked::detach)
                    .unwrap_or_else(|| crate::invariant::fail_stop())
            },
        )?;
        Some(batch.map(|batch| batch.after_unlock(self)))
    }

    pub fn seal_and_wait(&self) -> ClosedRetirement<'_, N, Q> {
        ClosedRetirement {
            owner: self,
            certificate: self.domain.seal_and_wait(),
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
        let queue = Mutex::new(vec![Probe(Arc::clone(&drops))]);
        let batch = DetachedLocked::detach(queue.lock());
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
        let queue = Mutex::new(vec![Probe(Arc::clone(&drops))]);
        let batch = DetachedLocked::detach(queue.lock());
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
