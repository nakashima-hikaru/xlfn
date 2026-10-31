//! Issuer-bound rotation administration. Dynamic pending state is inspected
//! once under its own lock; only the resulting consuming authority can drain,
//! issue a scoped certificate, and clear that same pending generation.
use super::*;

/// A drained generation valid only during its transition callback.
///
/// The certificate borrows the transition state, preventing it from escaping
/// the callback and authorizing reclamation after a later generation reuse.
/// Retirement subsystems receive this proof bundled with the issuing queues
/// through [`RetiredQueue::take`], without re-pairing a domain and certificate.
///
/// ```compile_fail
/// use xlfn_kernel::rotating_read_domain::RotatingReadDomain;
/// let domain = RotatingReadDomain::<1>::new();
/// let escaped = domain.quiesce(|certificate| certificate);
/// ```
#[derive(Debug)]
pub struct DrainedGeneration<'drain> {
    index: GenerationIndex,
    domain: &'drain AtomicUsize,
    transition: std::marker::PhantomData<&'drain mut Option<GenerationIndex>>,
}

impl DrainedGeneration<'_> {
    pub(super) fn generation(&self) -> GenerationIndex {
        self.index
    }

    /// Locks and takes a generation queue only after checking the issuing domain.
    /// The supplied array must be this domain's retirement queues. The
    /// certificate selects and locks the authorized array element itself;
    /// `take_queue` receives its guard and runs exactly once.
    pub(crate) fn take_queue<const N: usize, Q, R>(
        &self,
        domain: &RotatingReadDomain<N>,
        queues: &[Mutex<Q>; 2],
        take_queue: impl FnOnce(MutexGuard<'_, Q>) -> R,
    ) -> Option<R> {
        protocol::take_authorized_queue!(index, guard;
            self.index_for(domain), queues[index].lock(), take_queue(guard))
    }

    /// Returns the retirement queue index only for the issuing domain.
    #[must_use]
    pub(crate) fn index_for<const N: usize>(
        &self,
        domain: &RotatingReadDomain<N>,
    ) -> Option<usize> {
        protocol::authorize_domain!(self.domain, &domain.current, self.index.index())
    }
}

/// Permanent closure and both-generation drain for one borrowed domain.
/// Unlike a rotating certificate, this remains valid after the transition lock
/// is released: closed domains cannot reopen.
///
/// ```compile_fail
/// use xlfn_kernel::rotating_read_domain::RotatingReadDomain;
/// let closed = {
///     let domain = RotatingReadDomain::<1>::new();
///     domain.seal_and_wait()
/// };
/// ```
#[derive(Debug)]
pub struct ClosedDomain<'domain> {
    domain: &'domain AtomicUsize,
}

impl ClosedDomain<'_> {
    /// Locks both supplied retirement queues in index order and transfers both
    /// guards to the take callback. The supplied array must belong to this
    /// domain. The callback does not run for a foreign domain certificate.
    pub(crate) fn take_queues<const N: usize, Q, R>(
        &self,
        domain: &RotatingReadDomain<N>,
        queues: &[Mutex<Q>; 2],
        take_queues: impl FnOnce(MutexGuard<'_, Q>, MutexGuard<'_, Q>) -> R,
    ) -> Option<R> {
        protocol::take_authorized_queues!(indices, first, second;
            self.indices_for(domain), queues[indices[0]].lock(), queues[indices[1]].lock(),
            take_queues(first, second))
    }

    /// Authorizes both retirement queues only for the issuing domain.
    #[must_use]
    pub(crate) fn indices_for<const N: usize>(
        &self,
        domain: &RotatingReadDomain<N>,
    ) -> Option<[usize; 2]> {
        protocol::authorize_domain!(self.domain, &domain.current, [0, 1])
    }
}

pub(super) struct LockedDomain<'domain, const N: usize> {
    domain: &'domain RotatingReadDomain<N>,
    pending: MutexGuard<'domain, Option<GenerationIndex>>,
}

pub(super) enum RotationStart<'domain, const N: usize> {
    New(NewRotation<'domain, N>),
    Pending(PendingRotation<'domain, N>),
}

pub(super) struct NewRotation<'domain, const N: usize> {
    authority: LockedDomain<'domain, N>,
    old: GenerationIndex,
}

#[must_use = "drain or leave this rotation pending for a later call"]
pub(super) struct PendingRotation<'domain, const N: usize> {
    authority: LockedDomain<'domain, N>,
    old: GenerationIndex,
}

#[must_use = "run retirement before allowing this generation to be reused"]
pub(super) struct DrainedRotation<'domain, const N: usize> {
    authority: LockedDomain<'domain, N>,
    old: GenerationIndex,
}

impl<'domain, const N: usize> LockedDomain<'domain, N> {
    fn lock(domain: &'domain RotatingReadDomain<N>) -> Result<Self, DomainClosed> {
        let pending = domain.transition.lock();
        if domain.closed.load(Ordering::Acquire) {
            return Err(DomainClosed);
        }
        Ok(Self { domain, pending })
    }

    fn try_lock(domain: &'domain RotatingReadDomain<N>) -> Option<Result<Self, DomainClosed>> {
        let pending = domain.transition.try_lock()?;
        if domain.closed.load(Ordering::Acquire) {
            return Some(Err(DomainClosed));
        }
        Some(Ok(Self { domain, pending }))
    }

    pub(super) fn classify(self) -> RotationStart<'domain, N> {
        if let Some(old) = *self.pending {
            RotationStart::Pending(PendingRotation {
                authority: self,
                old,
            })
        } else {
            let old = self.domain.current_generation();
            RotationStart::New(NewRotation {
                authority: self,
                old,
            })
        }
    }

    fn rotate<B, R>(
        self,
        results: &mut [Option<R>; 2],
        barrier: impl FnOnce(GenerationIndex) -> B,
        mut operation: impl for<'drain> FnMut(DrainedGeneration<'drain>) -> R,
    ) {
        let new = match self.classify() {
            RotationStart::Pending(pending) => {
                let (authority, result) = pending.wait().run(&mut operation);
                results[0] = Some(result);
                match authority.classify() {
                    RotationStart::New(new) => new,
                    RotationStart::Pending(_) => crate::invariant::fail_stop(),
                }
            }
            RotationStart::New(new) => new,
        };
        let (_, result) = new.begin(barrier, || {}).wait().run(operation);
        results[1] = Some(result);
    }
}

impl<'domain, const N: usize> NewRotation<'domain, N> {
    fn begin<B>(
        mut self,
        barrier: impl FnOnce(GenerationIndex) -> B,
        between_publish_steps: impl FnOnce(),
    ) -> PendingRotation<'domain, N> {
        let barrier = barrier(self.old);
        let next = self.old.index() ^ 1;
        let replacement = self.authority.domain.generations[next]
            .control()
            .try_drained()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        let old = self.authority.domain.generations[self.old.index()].control();
        protocol::begin_rotation!(
            drop(old.seal()),
            *self.authority.pending = Some(self.old),
            publish_then_release_barrier(barrier, |_| publish_then_reopen(
                || self.authority.domain.current.store(next, Ordering::Release),
                || drop(replacement.reopen()),
                between_publish_steps
            ))
        );
        PendingRotation {
            authority: self.authority,
            old: self.old,
        }
    }

    fn try_begin<B>(
        mut self,
        barrier: impl FnOnce(GenerationIndex) -> Option<B>,
        idle_only: bool,
        before_seal: impl FnOnce(),
    ) -> Option<PendingRotation<'domain, N>> {
        let barrier = barrier(self.old)?;
        let next = self.old.index() ^ 1;
        let replacement = self.authority.domain.generations[next]
            .try_control()?
            .try_drained()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        let old = self.authority.domain.generations[self.old.index()].try_control()?;
        before_seal();
        if idle_only {
            protocol::try_begin_idle_rotation!(
                old.try_seal_if_idle().map(drop).is_ok(),
                *self.authority.pending = Some(self.old),
                publish_then_release_barrier(barrier, |_| publish_then_reopen(
                    || self.authority.domain.current.store(next, Ordering::Release),
                    || drop(replacement.reopen()),
                    || {}
                ))
            );
        } else {
            protocol::begin_rotation!(
                drop(old.seal()),
                *self.authority.pending = Some(self.old),
                publish_then_release_barrier(barrier, |_| publish_then_reopen(
                    || self.authority.domain.current.store(next, Ordering::Release),
                    || drop(replacement.reopen()),
                    || {}
                ))
            );
        }
        Some(PendingRotation {
            authority: self.authority,
            old: self.old,
        })
    }
}

impl<'domain, const N: usize> PendingRotation<'domain, N> {
    pub(super) fn wait(self) -> DrainedRotation<'domain, N> {
        drop(
            self.authority.domain.generations[self.old.index()]
                .control()
                .seal()
                .wait(),
        );
        DrainedRotation {
            authority: self.authority,
            old: self.old,
        }
    }

    pub(super) fn try_wait(self) -> Option<DrainedRotation<'domain, N>> {
        let control = self.authority.domain.generations[self.old.index()].try_control()?;
        let observed: Option<Result<(), DomainClosed>> =
            protocol::try_finish_rotation!(control.try_drained().map(drop).is_ok(), ());
        observed?.ok()?;
        Some(DrainedRotation {
            authority: self.authority,
            old: self.old,
        })
    }
}

impl<'domain, const N: usize> DrainedRotation<'domain, N> {
    fn certificate(&mut self) -> DrainedGeneration<'_> {
        DrainedGeneration {
            index: self.old,
            domain: &self.authority.domain.current,
            transition: std::marker::PhantomData,
        }
    }

    pub(super) fn run<R>(
        mut self,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
    ) -> (LockedDomain<'domain, N>, R) {
        let result = protocol::finish_rotation!(result;
            operation(self.certificate()), *self.authority.pending = None);
        (self.authority, result)
    }
}

impl<const N: usize> RotatingReadDomain<N> {
    /// Rotates the read generation and runs `operation` after the old
    /// generation is sealed and idle.
    ///
    /// The transition lock remains held while `operation` runs. Subsystems
    /// must use this callback to drain retired work before another rotation is
    /// allowed. If a polled transition is pending, its callback runs first,
    /// then the generation current at this call is rotated and drained. The
    /// fixed result slots retain both batches without allocating or dropping
    /// either callback result while the transition lock is held.
    pub fn quiesce<R>(
        &self,
        operation: impl for<'drain> FnMut(DrainedGeneration<'drain>) -> R,
    ) -> Result<[Option<R>; 2], DomainClosed> {
        let mut results = [None, None];
        LockedDomain::lock(self)?.rotate(&mut results, |_| (), operation);
        Ok(results)
    }

    /// Rotates while synchronizing deferred registration with publication.
    ///
    /// `barrier` acquires the old generation's retirement-queue guard while
    /// the transition lock is held. Holding it through next-generation
    /// publication makes earlier withdrawals visible to new readers. Writers
    /// that acquire the queue later must recheck the current generation.
    /// The guard is dropped before waiting for old readers or invoking the
    /// reclamation callback. Its acquisition/drop must not invoke user code.
    pub(crate) fn quiesce_with_publication_barrier<B, R>(
        &self,
        barrier: impl FnOnce(GenerationIndex) -> B,
        operation: impl for<'drain> FnMut(DrainedGeneration<'drain>) -> R,
    ) -> Result<[Option<R>; 2], DomainClosed> {
        let mut results = [None, None];
        LockedDomain::lock(self)?.rotate(&mut results, barrier, operation);
        Ok(results)
    }

    /// Best-effort form of [`Self::quiesce`] for maintenance paths that must
    /// not wait for another transition already in progress.
    ///
    /// Once the lock is acquired, this may wait for the old readers to drain.
    /// `None` means the transition lock was busy; `Some(Err(DomainClosed))`
    /// means the lock was acquired and closure was observed.
    pub fn try_quiesce<R>(
        &self,
        operation: impl for<'drain> FnMut(DrainedGeneration<'drain>) -> R,
    ) -> Option<Result<[Option<R>; 2], DomainClosed>> {
        let mut results = [None, None];
        let result = LockedDomain::try_lock(self)?;
        match result {
            Ok(authority) => {
                authority.rotate(&mut results, |_| (), operation);
                Some(Ok(results))
            }
            Err(error) => Some(Err(error)),
        }
    }

    /// Attempts quiescence without waiting for the transition lock or readers.
    ///
    /// Each stripe's idle check and admission seal are atomic. A reader that
    /// wins admission causes the attempt to return `None`, with the current
    /// generation left open. On success, `operation` runs under the transition
    /// lock after the old generation is sealed and idle. The callback itself
    /// may block; callers needing bounded latency must keep it non-blocking.
    /// If a polled transition is pending, only that transition is completed;
    /// its replacement is never reopened before its callback has run.
    pub fn try_quiesce_if_idle<R>(
        &self,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        self.try_quiesce_if_idle_impl(operation, || {})
    }

    /// Nonblocking retirement-registration form of idle quiescence.
    ///
    /// Return `None` from `barrier` if its guard cannot be acquired without
    /// waiting. No generation change occurs in that case. On success the
    /// guard follows the same publication ordering as
    /// [`Self::quiesce_with_publication_barrier`].
    pub(crate) fn try_quiesce_if_idle_with_publication_barrier<B, R>(
        &self,
        barrier: impl FnOnce(GenerationIndex) -> Option<B>,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        self.try_idle_with_barrier_impl(barrier, operation, || {})
    }

    /// Starts or completes a grace period without waiting for readers.
    ///
    /// Unlike idle-only maintenance, this seals the old generation and
    /// publishes its replacement even while old readers remain. Later readers
    /// cannot prolong that grace period. `None` means admission progressed
    /// but the old readers are still active, or a lock/barrier was busy.
    /// A subsequent poll, idle attempt or blocking quiescence must run the
    /// old generation's callback before its gate may be reused.
    pub(crate) fn poll_quiesce_with_publication_barrier<B, R>(
        &self,
        barrier: impl FnOnce(GenerationIndex) -> Option<B>,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        let authority = match LockedDomain::try_lock(self)? {
            Ok(authority) => authority,
            Err(error) => return Some(Err(error)),
        };
        let pending = match authority.classify() {
            RotationStart::Pending(pending) => pending,
            RotationStart::New(new) => new.try_begin(barrier, false, || {})?,
        };
        let (_, result) = pending.try_wait()?.run(operation);
        Some(Ok(result))
    }

    pub(super) fn try_quiesce_if_idle_impl<R>(
        &self,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
        before_seal: impl FnOnce(),
    ) -> Option<Result<R, DomainClosed>> {
        self.try_idle_with_barrier_impl(|_| Some(()), operation, before_seal)
    }

    pub(super) fn try_idle_with_barrier_impl<B, R>(
        &self,
        barrier: impl FnOnce(GenerationIndex) -> Option<B>,
        operation: impl for<'drain> FnOnce(DrainedGeneration<'drain>) -> R,
        before_seal: impl FnOnce(),
    ) -> Option<Result<R, DomainClosed>> {
        let authority = match LockedDomain::try_lock(self)? {
            Ok(authority) => authority,
            Err(error) => return Some(Err(error)),
        };
        let pending = match authority.classify() {
            RotationStart::Pending(pending) => pending,
            RotationStart::New(new) => new.try_begin(barrier, true, before_seal)?,
        };
        let (_, result) = pending.try_wait()?.run(operation);
        Some(Ok(result))
    }

    #[cfg(test)]
    pub(super) fn test_publish_reopen_window<R>(&self, operation: impl FnOnce() -> R) -> R {
        let authority = LockedDomain::lock(self).unwrap();
        let RotationStart::New(new) = authority.classify() else {
            panic!("publication-window test requires no pending rotation")
        };
        let mut result = None;
        let pending = new.begin(|_| (), || result = Some(operation()));
        drop(pending.wait());
        result.unwrap()
    }

    /// Permanently closes the domain and waits for both generations to drain.
    pub fn seal_and_wait(&self) -> ClosedDomain<'_> {
        let _transition = self.transition.lock();
        protocol::close_domain!(
            self.closed.store(true, Ordering::Release),
            drop(self.generations[0].control().seal().wait()),
            drop(self.generations[1].control().seal().wait())
        );
        ClosedDomain {
            domain: &self.current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_abandoned_pending_rotation_resumes_without_reusing_its_generation() {
        let domain = RotatingReadDomain::<2>::new();
        let RotationStart::New(new) = LockedDomain::lock(&domain).unwrap().classify() else {
            panic!("fresh domain cannot have a pending rotation")
        };
        drop(new.begin(|_| (), || {}));
        assert_eq!(domain.current_generation().index(), 1);
        assert_eq!(
            domain.poll_quiesce_with_publication_barrier(
                |_| Some(()),
                |proof| proof.index_for(&domain).unwrap()
            ),
            Some(Ok(0))
        );
        assert_eq!(domain.current_generation().index(), 1);
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "adversarial test of safe authority forgetting; no retirement may be authorized"
    )]
    fn miri_forgotten_rotation_authority_stops_reuse_without_issuing_retirement() {
        for drained in [false, true] {
            let domain = RotatingReadDomain::<2>::new();
            let RotationStart::New(new) = LockedDomain::lock(&domain).unwrap().classify() else {
                panic!("fresh domain cannot have a pending rotation")
            };
            let pending = new.begin(|_| (), || {});
            if drained {
                std::mem::forget(pending.wait());
            } else {
                std::mem::forget(pending);
            }
            assert_eq!(domain.current_generation().index(), 1);
            assert!(domain.transition.try_lock().is_none());
            assert!(
                domain
                    .try_quiesce_if_idle(|_| panic!(
                        "forgotten authority cannot authorize retirement"
                    ))
                    .is_none()
            );
            let reader = domain.enter(0).unwrap();
            drop(reader);
        }
    }
}
