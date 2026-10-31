//! Scoped withdrawal capabilities retain their issuing queue owner. Consumers
//! supply the take operation, never another domain, generation index or queue.
use super::*;

#[must_use = "retired work remains queued until the scoped withdrawal runs"]
pub struct RetiredQueue<'drain, const N: usize, Q> {
    owner: &'drain RotatingRetirementDomain<N, Q>,
    certificate: DrainedGeneration<'drain>,
}

impl<const N: usize, Q> RetiredQueue<'_, N, Q> {
    pub fn belongs_to(&self, owner: &RotatingRetirementDomain<N, Q>) -> bool {
        std::ptr::eq(self.owner, owner)
    }

    pub fn generation(&self) -> GenerationIndex {
        self.certificate.generation()
    }

    /// Consumes this generation's withdrawal authority. The held transition
    /// lock excludes reuse until the callback and its queue transfer finish.
    pub fn take<R>(self, take: impl FnOnce(MutexGuard<'_, Q>) -> R) -> R {
        self.certificate
            .take_queue(&self.owner.domain, &self.owner.queues, take)
            .unwrap_or_else(|| crate::invariant::fail_stop())
    }
}

/// Permanent closure observation bound to its two retirement queues. Repeated
/// terminal observations may take already-empty queues; allocation ownership
/// itself still transfers only once from each protected queue entry.
pub struct ClosedRetirement<'owner, const N: usize, Q> {
    owner: &'owner RotatingRetirementDomain<N, Q>,
    certificate: ClosedDomain<'owner>,
}

impl<const N: usize, Q> ClosedRetirement<'_, N, Q> {
    pub fn belongs_to(&self, owner: &RotatingRetirementDomain<N, Q>) -> bool {
        std::ptr::eq(self.owner, owner)
    }

    pub fn take_queues<R>(
        &self,
        take: impl FnOnce(MutexGuard<'_, Q>, MutexGuard<'_, Q>) -> R,
    ) -> R {
        self.certificate
            .take_queues(&self.owner.domain, &self.owner.queues, take)
            .unwrap_or_else(|| crate::invariant::fail_stop())
    }
}

impl<const N: usize, Q> RotatingRetirementDomain<N, Q> {
    /// Rotates with the old queue locked through publication, then passes the
    /// issuing queue in a non-escaping, consuming withdrawal capability.
    pub fn quiesce<R>(
        &self,
        mut operation: impl for<'drain> FnMut(RetiredQueue<'drain, N, Q>) -> R,
    ) -> Result<[Option<R>; 2], DomainClosed> {
        self.domain.quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].lock(),
            |certificate| {
                operation(RetiredQueue {
                    owner: self,
                    certificate,
                })
            },
        )
    }

    pub fn try_quiesce_if_idle<R>(
        &self,
        operation: impl for<'drain> FnOnce(RetiredQueue<'drain, N, Q>) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        self.domain.try_quiesce_if_idle_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                operation(RetiredQueue {
                    owner: self,
                    certificate,
                })
            },
        )
    }

    pub fn poll_quiesce<R>(
        &self,
        operation: impl for<'drain> FnOnce(RetiredQueue<'drain, N, Q>) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        self.domain.poll_quiesce_with_publication_barrier(
            |generation| self.queues[generation.index()].try_lock(),
            |certificate| {
                operation(RetiredQueue {
                    owner: self,
                    certificate,
                })
            },
        )
    }

    pub fn seal_and_wait(&self) -> ClosedRetirement<'_, N, Q> {
        ClosedRetirement {
            owner: self,
            certificate: self.domain.seal_and_wait(),
        }
    }
}

impl<const N: usize, Q> Drop for RotatingRetirementDomain<N, Q> {
    fn drop(&mut self) {
        // End admission and its notification tail before queued owners are
        // destroyed. This does not relax detached permits' owner-lifetime
        // contract: that owner must still be retained through final release
        // before recovering a Box or starting its exclusive destruction.
        // Every internal guard ends before queue payload field destructors.
        let _closed = self.domain.seal_and_wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize};

    #[test]
    #[allow(
        clippy::mem_forget,
        clippy::forget_non_drop,
        reason = "adversarial test that forgetting a borrowed withdrawal never drops queued owners"
    )]
    fn miri_forgotten_queue_withdrawal_retains_payload_until_terminal_drain() {
        struct Probe(Arc<AtomicUsize>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let domain = RotatingRetirementDomain::<2, Vec<Probe>>::new();
        domain.register_retired(|_, mut queue| queue.push(Probe(Arc::clone(&drops))));
        domain.quiesce(|retired| std::mem::forget(retired)).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        domain.seal_and_wait().take_queues(|mut first, mut second| {
            drop(std::mem::take(&mut *first));
            drop(std::mem::take(&mut *second));
        });
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_retirement_owner_drop_destroys_queued_payload_after_terminal_drain() {
        struct Probe(Arc<AtomicUsize>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = RotatingRetirementDomain::<2, Vec<Probe>>::new();
        let reader = owner.enter(0).unwrap();
        owner.register_retired(|_, mut queue| queue.push(Probe(Arc::clone(&drops))));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(reader);
        let closed = owner.seal_and_wait();
        assert!(closed.belongs_to(&owner));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
