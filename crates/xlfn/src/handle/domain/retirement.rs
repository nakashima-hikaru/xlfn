//! Handle-specific ownership and destruction accounting over kernel retirement.
//!
//! Only this module can unwrap kernel batches or change queued/debt counts.
//! Its caller receives an opaque batch tied to this exact accounting owner;
//! forgetting that batch retains both its records and its destruction debt.

use super::{counters, protocol};
use crate::handle::binding::BindingRecord;
use crate::sync::{Condvar, Mutex};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::published_owner::PublishedOwner;
#[cfg(test)]
use xlfn_kernel::rotating_read_domain::RotatingReadPermit;
use xlfn_kernel::rotating_read_domain::{
    DomainClosed, RotatingReadOwnedPermit, RotatingRetirementDomain,
};

// Single-cell deletion and ordinary revision replacement should not allocate
// a heap queue. Larger retirement batches retain their allocation when drained.
type PendingBindings = SmallVec<[UnlinkedBindingOwner; 4]>;

/// A withdrawn allocation still protected by its reader generation. Only this
/// adapter can recover it after its embedded domain returns a disposable batch.
pub(crate) struct UnlinkedBindingOwner {
    owner: ManuallyDrop<PublishedOwner<BindingRecord>>,
}
impl UnlinkedBindingOwner {
    pub(in crate::handle) fn retain(owner: ManuallyDrop<PublishedOwner<BindingRecord>>) -> Self {
        Self { owner }
    }
}

pub(super) struct RetirementAccounting {
    domain: RotatingRetirementDomain<DEFAULT_STRIPE_COUNT, PendingBindings>,
    // Maintenance hint; queue locks and the driver handoff publish work.
    queued: AtomicUsize,
    // Release discharge synchronizes with completion's Acquire observation.
    debt: AtomicUsize,
    completion: Mutex<()>,
    changed: Condvar,
}

/// Owns only records detached after the kernel's queue/transition locks end.
/// The borrow fixes the issuing domain, accounting owner and lifetime together.
#[must_use = "reclaim batches retain destruction debt until dropped"]
pub(super) struct ReclaimBatch<'domain> {
    owner: &'domain RetirementAccounting,
    records: PendingBindings,
}

impl<'domain> ReclaimBatch<'domain> {
    pub(super) fn reclaim(self) {
        drop(self);
    }
}

impl Drop for ReclaimBatch<'_> {
    fn drop(&mut self) {
        // Empty before user code: reentry and merged source batches cannot
        // discharge the same records twice. No debt guard runs on unwind.
        let records = std::mem::take(&mut self.records);
        if records.is_empty() {
            return;
        }
        let owner = self.owner;
        let count = records.len();
        let address = std::ptr::from_ref(owner).addr();
        RECLAIMING.with(|stack| stack.borrow_mut().push(address));
        let _guard = scopeguard::guard(address, |address| {
            RECLAIMING.with(|stack| assert_eq!(stack.borrow_mut().pop(), Some(address)));
        });
        // Kernel locks ended before batch construction. The caller must also
        // end its table writer guard before polling or dropping this batch.
        protocol::complete_reclamation!(_destroyed, completion;
            {
                for record in records {
                    drop(ManuallyDrop::into_inner(record.owner));
                }
            },
            update_count(&owner.debt, count, Ordering::Release, counters::subtract),
            owner.completion.lock(),
            owner.changed.notify_all(),
            drop(completion)
        );
    }
}

impl RetirementAccounting {
    pub(super) fn new() -> Self {
        Self {
            domain: RotatingRetirementDomain::new(),
            queued: AtomicUsize::new(0),
            debt: AtomicUsize::new(0),
            completion: Mutex::new(()),
            changed: Condvar::new(),
        }
    }

    #[cfg(test)]
    #[inline]
    pub(super) fn enter_current_thread(
        &self,
    ) -> Result<RotatingReadPermit<'_, DEFAULT_STRIPE_COUNT>, DomainClosed> {
        self.domain.enter_current_thread()
    }

    /// The caller keeps this owner stationary and alive through permit release.
    #[inline]
    pub(super) unsafe fn enter_owned_current_thread(
        &self,
    ) -> Result<RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>, DomainClosed> {
        // SAFETY: the outer handle permit retains the caller's lifetime contract.
        unsafe { self.domain.enter_owned_current_thread() }
    }

    #[inline]
    pub(super) fn current_thread_may_be_reading(&self) -> bool {
        self.domain.current_thread_may_be_reading()
    }

    /// The table writer holds publication withdrawal through this operation.
    /// The selected queue lock covers append and both accounting updates, so a
    /// rotation cannot detach a record whose registration is not yet charged.
    pub(super) fn register(&self, record: UnlinkedBindingOwner) {
        self.domain.register_retired(|_, mut queue| {
            queue.append(record);
            update_count(&self.debt, 1, Ordering::Relaxed, counters::add);
            update_count(&self.queued, 1, Ordering::Relaxed, counters::add);
        });
    }

    pub(super) fn queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    pub(super) fn debt(&self) -> usize {
        self.debt.load(Ordering::Acquire)
    }

    pub(super) fn is_reclaiming_here(&self) -> bool {
        let address = std::ptr::from_ref(self).addr();
        RECLAIMING.with(|stack| stack.borrow().contains(&address))
    }

    // No caller supplies a batch, count, or a second accounting owner. Only
    // polling this embedded kernel domain can reach the conversion boundary.
    fn account_records(&self, records: PendingBindings) -> ReclaimBatch<'_> {
        update_count(
            &self.queued,
            records.len(),
            Ordering::Relaxed,
            counters::subtract,
        );
        ReclaimBatch {
            owner: self,
            records,
        }
    }

    pub(super) fn poll_reclaim(&self) -> Option<ReclaimBatch<'_>> {
        let records = self.domain.poll_quiesce()?.ok()?.into_inner();
        Some(self.account_records(records))
    }

    pub(super) fn quiesce_reclaim(&self) -> ReclaimBatch<'_> {
        let records = self
            .domain
            .quiesce()
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(|batch| batch.into_inner())
            .filter(|records| !records.is_empty())
            .reduce(|mut first, mut second| {
                first.append(&mut second);
                first
            })
            .unwrap_or_default();
        self.account_records(records)
    }

    pub(super) fn close_reclaim(&self) -> ReclaimBatch<'_> {
        let closed = self.domain.seal_and_wait();
        let [first, second] = closed.take_queues();
        let mut records = first.into_inner();
        records.append(&mut second.into_inner());
        self.account_records(records)
    }

    pub(super) fn wait_for_completion(&self) {
        let mut completion = self.completion.lock();
        while self.debt() != 0 {
            self.changed.wait(&mut completion);
        }
    }
}

// Atomic semantics remain the primitive boundary. Both the update closure and
// returned value use the same checked, side-effect-free transition kernel.
#[inline]
fn update_count(
    counter: &AtomicUsize,
    amount: usize,
    ordering: Ordering,
    step: impl Fn(usize, usize) -> counters::CountStep<usize>,
) -> usize {
    let previous = counter
        .try_update(ordering, Ordering::Relaxed, |state| {
            match step(state, amount) {
                counters::CountStep::Success(next) => Some(next),
                counters::CountStep::FailStop => None,
            }
        })
        .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
    match step(previous, amount) {
        counters::CountStep::Success(next) => next,
        counters::CountStep::FailStop => xlfn_kernel::invariant::fail_stop(),
    }
}

thread_local! {
    static RECLAIMING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::registry::HandleRegistry;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "exercise safe abandonment without certifying destruction"
    )]
    #[cfg_attr(
        miri,
        ignore = "intentionally leaks the forgotten batch and its registry owner"
    )]
    fn forgotten_batch_retains_payload_and_destruction_debt() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let owner = &registry.bindings.read_domain().retirement;
        let reader = owner.domain.enter(0).unwrap();
        registry.remove::<Counted>(&token).unwrap();
        drop(reader);
        let batch = owner.quiesce_reclaim();
        assert_eq!(owner.queued(), 0);
        assert_eq!(owner.debt(), 1);
        std::mem::forget(batch);
        owner.close_reclaim().reclaim();
        assert_eq!(
            owner.debt(),
            1,
            "closing cannot certify forgotten destruction"
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        // Final registry teardown correctly waits forever on this debt. Keep
        // the owner alive along with its deliberately forgotten batch.
        std::mem::forget(registry);
    }

    #[test]
    fn miri_panicking_payload_finishes_fields_before_batch_debt_discharge() {
        struct Field {
            registry: std::sync::Weak<HandleRegistry>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Field {
            fn drop(&mut self) {
                let registry = self.registry.upgrade().unwrap();
                let domain = registry.bindings.read_domain();
                assert_eq!(domain.debt(), 1);
                assert!(domain.is_reclaiming_here());
                assert!(registry.bindings.try_read_state().is_some());
                assert!(domain.retirement.domain.try_queue_lens().is_some());
                self.drops.fetch_add(1, Ordering::Release);
            }
        }
        struct Panicking {
            _field: Field,
        }
        impl Drop for Panicking {
            fn drop(&mut self) {
                panic!("application payload destructor panic");
            }
        }
        let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Panicking {
                _field: Field {
                    registry: Arc::downgrade(&registry),
                    drops: Arc::clone(&drops),
                },
            }))
            .unwrap();
        let owner = &registry.bindings.read_domain().retirement;
        let reader = owner.domain.enter(0).unwrap();
        registry.remove::<Panicking>(&token).unwrap();
        drop(reader);
        owner.quiesce_reclaim().reclaim();
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(owner.debt(), 0);
        assert!(!owner.is_reclaiming_here());
        assert!(matches!(
            registry.objects.finish_quiescence(),
            Err(crate::XllError::Panic)
        ));
    }

    #[test]
    fn seal_waits_for_detached_batch_destruction_completion() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let domain = registry.bindings.read_domain();
        let reader = domain.retirement.domain.enter(0).unwrap();
        registry.remove::<Counted>(&token).unwrap();
        drop(reader);
        let batch = domain.retirement.quiesce_reclaim();
        assert_eq!(domain.retirement.queued(), 0);
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (finished_tx, finished_rx) = std::sync::mpsc::channel();
            let sealer = scope.spawn(move || {
                started_tx.send(()).unwrap();
                domain.seal();
                finished_tx.send(drops.load(Ordering::Acquire)).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(finished_rx.recv_timeout(Duration::from_millis(20)).is_err());
            assert_eq!(domain.debt(), 1);
            drop(batch);
            assert_eq!(
                finished_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
                1
            );
            sealer.join().unwrap();
        });
        assert_eq!(domain.debt(), 0);
    }

    #[test]
    fn miri_blocking_quiescence_drives_retirement_queued_in_the_new_generation() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let domain = registry.bindings.read_domain();
        // Hold an initially empty generation across a blocking quiescence.
        // Use the kernel permit so its drop supplies no handle-level retry:
        // the blocking writer must itself service the new generation's debt.
        let permit = domain.retirement.domain.enter(0).unwrap();
        let initial = domain.retirement.domain.current_generation();
        let worker_registry = Arc::clone(&registry);
        let worker = std::thread::spawn(move || worker_registry.bindings.read_domain().quiesce());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while domain.retirement.domain.current_generation() == initial
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let rotated = domain.retirement.domain.current_generation() != initial;
        if rotated {
            registry.remove::<Counted>(&token).unwrap();
            assert_eq!(domain.debt(), 1);
            assert_eq!(drops.load(Ordering::Acquire), 0);
        }
        drop(permit);
        worker.join().unwrap();
        assert!(
            rotated,
            "the blocking writer must publish its next generation"
        );
        assert_eq!(domain.debt(), 0);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn miri_disposable_bindings_keep_debt_during_reentrant_destruction() {
        struct Reenter {
            registry: std::sync::Weak<HandleRegistry>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                let registry = self.registry.upgrade().unwrap();
                let domain = registry.bindings.read_domain();
                assert_eq!(domain.debt(), 1, "destruction still owns its debt");
                assert!(registry.bindings.try_read_state().is_some());
                assert!(domain.retirement.domain.try_queue_lens().is_some());
                assert!(domain.retirement.domain.try_quiesce_if_idle().is_some());
                domain.maintain();
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Reenter {
                registry: Arc::downgrade(&registry),
                drops: Arc::clone(&drops),
            }))
            .unwrap();
        let domain = registry.bindings.read_domain();
        let reader = domain.retirement.domain.enter(0).unwrap();
        registry.remove::<Reenter>(&token).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(reader);
        let batch = domain.retirement.quiesce_reclaim();
        assert_eq!(domain.retirement.queued(), 0);
        assert_eq!(domain.debt(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(batch);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(domain.debt(), 0);
    }

    #[test]
    fn miri_drained_binding_batches_merge_without_discharging_early() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let registry = HandleRegistry::from_entropy(2, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let first = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let second = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let domain = registry.bindings.read_domain();
        // This kernel permit does not perform Handle maintenance on drop.
        let permit = domain.retirement.domain.enter(0).unwrap();
        registry.remove::<Counted>(&first).unwrap();
        registry.remove::<Counted>(&second).unwrap();
        drop(permit);
        let batch = domain.retirement.quiesce_reclaim();
        assert_eq!(domain.retirement.queued(), 0);
        assert_eq!(
            domain.debt(),
            2,
            "merged generations retain all completion debt"
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        // RAII completion must work even without calling reclaim explicitly.
        drop(batch);
        assert_eq!(domain.debt(), 0);
        assert_eq!(drops.load(Ordering::Acquire), 2);
    }
}
