//! Call-scoped handle read domain and deferred reclamation.
//!
//! Rather than acquiring a per-record reader permit on every lookup, UDF
//! calls register once with this domain for the duration of their [`CallScope`].
//! Readers enter striped drain gates partitioned by generation, avoiding
//! cache-line contention on individual binding records during concurrent
//! lookups of the same handle.
//!
//! Removed bindings enter generation-bound queues. Writers and departing
//! readers reclaim idle generations while borrowing the registry owner. The
//! last reader therefore reclaims even a single retirement without a worker
//! owning or extending the domain's lifetime. Writers apply blocking
//! backpressure at 256 records when they cannot hold their own read permit.
//! Final seal waits for reader and destructor completion.
//! Only the current generation is open, so a reader that observed the old
//! generation before rotation cannot be admitted after the grace period starts.

#![allow(
    unsafe_code,
    reason = "owned read permits are audited temporal capabilities"
)]

mod counters;
mod protocol;

use crate::{XllError, XllResult};
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    DrainedGeneration, RotatingReadDomain, RotatingReadOwnedPermit,
};

#[cfg(test)]
use xlfn_kernel::rotating_read_domain::RotatingReadPermit;

use super::binding::BindingRecord;
use parking_lot::{Condvar, Mutex};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn_kernel::published_owner::PublishedOwner;

// Atomic semantics remain the primitive boundary. Both the update closure and
// returned value use the same checked, side-effect-free transition kernel.
#[inline]
fn update_count(
    counter: &AtomicUsize,
    amount: usize,
    ordering: Ordering,
    step: fn(usize, usize) -> counters::CountStep<usize>,
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

// Single-cell deletion and ordinary revision replacement should not allocate
// a heap queue. Larger retirement batches retain their allocation when drained.
type PendingBindings = SmallVec<[PublishedOwner<BindingRecord>; 4]>;

pub(crate) struct HandleReadDomain {
    domain: RotatingReadDomain<DEFAULT_STRIPE_COUNT>,
    pending: [Mutex<PendingBindings>; 2],
    // Maintenance hint; pending queue locks and the driver handoff publish work.
    queued: AtomicUsize,
    // Also a destruction-completion counter: release decrements synchronize
    // with seal/flush Acquire loads, even before a reclaimer locks completion.
    debt: AtomicUsize,
    peak_debt: AtomicUsize,
    maintenance_requests: AtomicUsize,
    completion: Mutex<()>,
    changed: Condvar,
}

/// Owns only records removed through a drained/closed domain certificate.
/// The domain borrow fixes both destruction-completion accounting and lifetime.
#[must_use = "drained bindings retain destruction debt until dropped"]
struct DrainedBindings<'domain> {
    domain: &'domain HandleReadDomain,
    records: PendingBindings,
}

impl<'domain> DrainedBindings<'domain> {
    fn empty(domain: &'domain HandleReadDomain) -> Self {
        Self {
            domain,
            records: SmallVec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn extend(&mut self, mut other: Self) {
        protocol::append_owned_batch!(
            std::ptr::from_ref(self.domain), std::ptr::from_ref(other.domain);
            xlfn_kernel::invariant::fail_stop(),
            self.records.append(&mut other.records)
        );
    }

    fn reclaim(self) {
        drop(self);
    }
}

impl Drop for DrainedBindings<'_> {
    fn drop(&mut self) {
        // Taking empties the wrapper before user code, so even a nested cleanup
        // cannot discharge this batch twice. Merging leaves the source empty.
        let records = std::mem::take(&mut self.records);
        if records.is_empty() {
            return;
        }
        let domain = self.domain;
        let count = records.len();
        let address = std::ptr::from_ref(domain).addr();
        RECLAIMING.with(|stack| stack.borrow_mut().push(address));
        let _guard = scopeguard::guard(address, |address| {
            RECLAIMING.with(|stack| assert_eq!(stack.borrow_mut().pop(), Some(address)));
        });
        // Call sites drop batches after table, queue and transition guards end.
        protocol::complete_reclamation!(_destroyed, completion;
            drop(records),
            update_count(&domain.debt, count, Ordering::Release, counters::subtract),
            domain.completion.lock(),
            domain.changed.notify_all(),
            drop(completion)
        );
    }
}

pub(crate) struct HandleDomainPermit {
    domain: NonNull<HandleReadDomain>,
    permit: Option<RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>>,
}

impl HandleDomainPermit {
    pub(crate) fn domain(&self) -> NonNull<HandleReadDomain> {
        self.domain
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn witness(&self) -> crate::call::HandleDomainWitness<'_> {
        crate::call::HandleDomainWitness::from_permit(self)
    }
}

impl Drop for HandleDomainPermit {
    fn drop(&mut self) {
        drop(self.permit.take());
        // SAFETY: enter_owned requires the domain owner to outlive the whole
        // capability, including this final maintenance notification.
        unsafe { self.domain.as_ref() }.maintain_after_reader();
    }
}

#[cfg(test)]
pub(crate) struct HandleBindingDomainPermit<'domain> {
    domain: &'domain HandleReadDomain,
    permit: Option<RotatingReadPermit<'domain, DEFAULT_STRIPE_COUNT>>,
}

#[cfg(test)]
impl Drop for HandleBindingDomainPermit<'_> {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.domain.maintain_after_reader();
    }
}

impl HandleReadDomain {
    pub(crate) fn new() -> Self {
        Self {
            domain: RotatingReadDomain::new(),
            pending: [Mutex::new(SmallVec::new()), Mutex::new(SmallVec::new())],
            queued: AtomicUsize::new(0),
            debt: AtomicUsize::new(0),
            peak_debt: AtomicUsize::new(0),
            maintenance_requests: AtomicUsize::new(0),
            completion: Mutex::new(()),
            changed: Condvar::new(),
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn enter(&self) -> XllResult<HandleBindingDomainPermit<'_>> {
        self.domain
            .enter_current_thread()
            .map(|permit| HandleBindingDomainPermit {
                domain: self,
                permit: Some(permit),
            })
            .map_err(|_| XllError::Closing)
    }

    /// Enters the handle read domain and acquires an owned reader permit.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self` outlives the returned [`HandleDomainPermit`].
    /// All permits must be dropped before the domain is destroyed or moved.
    ///
    /// Protocol obligation [HD-1]: Reader admission under domain isolation guarantees safe access.
    #[inline]
    pub(crate) unsafe fn enter_owned(&self) -> XllResult<HandleDomainPermit> {
        // SAFETY: guaranteed by caller's owner-lifetime contract;
        // self outlives the returned permit and will be drained before reclamation.
        unsafe {
            self.domain
                .enter_owned_current_thread()
                .map(|permit| HandleDomainPermit {
                    domain: NonNull::from(self),
                    permit: Some(permit),
                })
                .map_err(|_| XllError::Closing)
        }
    }

    /// Withdrawal and enqueue share the binding-table writer lock with seal.
    /// Rotation acquires this queue before publishing the next generation.
    /// Either our withdrawal happens-before that publication, or acquiring the
    /// queue after publication makes the recheck below select the next queue.
    /// A double generation load without that queue/publication barrier would
    /// not exclude new readers observing a stale withdrawn pointer.
    ///
    /// Protocol obligation [HD-2]: Retired bindings enter generation-bound queue; immediate drop is forbidden.
    pub(crate) fn enqueue_reclaim(&self, record: PublishedOwner<BindingRecord>) {
        self.domain.register_retired(
            |generation| self.pending[generation.index()].lock(),
            |_, mut queue| {
                crate::retirement_queue::append_retired!(&mut *queue, record);
                let debt = update_count(&self.debt, 1, Ordering::Relaxed, counters::add);
                self.peak_debt.fetch_max(debt, Ordering::Relaxed);
                update_count(&self.queued, 1, Ordering::Relaxed, counters::add);
            },
        );
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn debt_snapshot(&self) -> (usize, usize, usize) {
        (
            self.queued.load(Ordering::Relaxed),
            self.debt(),
            self.peak_debt.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn debt(&self) -> usize {
        self.debt.load(Ordering::Acquire)
    }

    pub(crate) fn is_reclaiming_here(&self) -> bool {
        let address = std::ptr::from_ref(self).addr();
        RECLAIMING.with(|stack| stack.borrow().contains(&address))
    }

    pub(crate) fn maintain_after_removal(&self) {
        if self.is_reclaiming_here() {
            return;
        }
        let debt = self.debt();
        // CallScope is !Send and all handle permits use the acquiring thread's
        // default stripe. Looking at both generations therefore excludes an
        // own permit without changing lookup/permit-entry bookkeeping.
        if debt >= HARD_DEBT_LIMIT && !self.domain.current_thread_may_be_reading() {
            self.quiesce();
        } else {
            self.maintain();
        }
    }

    fn take_generation(&self, generation: DrainedGeneration<'_>) -> DrainedBindings<'_> {
        let records = generation
            .take_queue(
                &self.domain,
                |index| self.pending[index].lock(),
                |mut queue| crate::retirement_queue::take_retired!(&mut *queue),
            )
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        update_count(
            &self.queued,
            records.len(),
            Ordering::Relaxed,
            counters::subtract,
        );
        DrainedBindings {
            domain: self,
            records,
        }
    }

    fn maintain_after_reader(&self) {
        // The permit's Release RMW precedes this fence. If a writer already
        // sealed that stripe, acquire its Release sequence: enqueue precedes
        // seal, so the zero-queue test cannot miss that writer's debt. If this
        // reader left before seal, the writer's idle check sees its departure
        // and handles the drain. A queue load without this fence can lose the
        // final reader notification on weakly ordered hosts.
        std::sync::atomic::fence(Ordering::Acquire);
        if self.queued.load(Ordering::Relaxed) != 0 {
            self.maintain();
        }
    }

    pub(crate) fn maintain(&self) {
        if self.is_reclaiming_here() {
            return;
        }
        // Writers and blocking quiescence always register. In particular,
        // acquiring a previous failed poll's notification after releasing the
        // transition lock makes its queued work visible to this next pass.
        // Every requester registers once. Only the 0 -> 1 caller drives;
        // requests arriving during a poll remain in the count and force the
        // driver to acquire them before another poll. Subtracting the consumed
        // batch to zero transfers driving responsibility to the next caller.
        // One modification order replaces a two-flag handoff, so a departing
        // reader's notification cannot be cleared without being consumed.
        let previous = self
            .maintenance_requests
            .try_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                requests.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        if previous != 0 {
            return;
        }
        let mut consumed = 1;
        loop {
            self.poll_maintenance();
            let requested = self
                .maintenance_requests
                .fetch_sub(consumed, Ordering::AcqRel);
            if requested == consumed {
                return;
            }
            consumed = requested - consumed;
        }
    }

    fn poll_maintenance(&self) {
        while self.queued.load(Ordering::Relaxed) != 0 {
            let Some(Ok(records)) = self.domain.poll_quiesce_with_publication_barrier(
                |generation| self.pending[generation.index()].try_lock(),
                |generation| self.take_generation(generation),
            ) else {
                break;
            };
            records.reclaim();
        }
    }

    /// Rotates the domain and drains retired bindings.
    ///
    /// Protocol obligation [HD-3]: Generational quiescence ensures all readers of the retired generation have drained.
    pub(crate) fn quiesce(&self) {
        let records = self
            .domain
            .quiesce_with_publication_barrier(
                |generation| self.pending[generation.index()].lock(),
                |generation| self.take_generation(generation),
            )
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .filter(|batch| !batch.is_empty())
            .reduce(|mut records, next| {
                records.extend(next);
                records
            })
            .unwrap_or_else(|| DrainedBindings::empty(self));
        records.reclaim();
        self.maintain();
    }

    #[cfg(test)]
    pub(crate) fn flush_for_test(&self) {
        self.quiesce();
        let mut completion = self.completion.lock();
        while self.debt() != 0 {
            self.changed.wait(&mut completion);
        }
    }

    /// Final drain closes reader admission and waits for destruction already
    /// handed to another borrowing writer or departing reader.
    ///
    /// Protocol obligation [HD-5]: Destruction barrier: waits for domain drain and flushes remaining debt before arena teardown.
    pub(crate) fn seal(&self) {
        let closed = self.domain.seal_and_wait();
        let [mut records, remaining] = closed
            .take_queues(
                &self.domain,
                |index| self.pending[index].lock(),
                |mut first, mut second| {
                    let records = crate::retirement_queue::take_retired!(&mut *first);
                    update_count(
                        &self.queued,
                        records.len(),
                        Ordering::Relaxed,
                        counters::subtract,
                    );
                    let remaining = crate::retirement_queue::take_retired!(&mut *second);
                    update_count(
                        &self.queued,
                        remaining.len(),
                        Ordering::Relaxed,
                        counters::subtract,
                    );
                    [
                        DrainedBindings {
                            domain: self,
                            records,
                        },
                        DrainedBindings {
                            domain: self,
                            records: remaining,
                        },
                    ]
                },
            )
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        records.extend(remaining);
        records.reclaim();
        if !self.is_reclaiming_here() {
            let mut completion = self.completion.lock();
            while self.debt() != 0 {
                self.changed.wait(&mut completion);
            }
        }
    }
}

pub(crate) const HARD_DEBT_LIMIT: usize = 256;
thread_local! {
    static RECLAIMING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{ExcelHandleObject, registry::HandleRegistry};
    use std::sync::Arc;
    use std::time::Duration;

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn loom_empty_queue_fast_path_preserves_last_reader_notification() {
        use loom::sync::Arc;
        use loom::sync::atomic::{AtomicUsize, Ordering, fence};

        struct Maintenance {
            // Bits 1/2 are seal/wait flags. Keep the same RMW orderings
            // as SealableCounter::release / seal / mark_waiting.
            gates: [AtomicUsize; 2],
            queued: AtomicUsize,
            requests: AtomicUsize,
        }

        impl Maintenance {
            fn maintain(&self) {
                if self.requests.fetch_add(1, Ordering::AcqRel) != 0 {
                    return;
                }
                let mut consumed = 1;
                loop {
                    if self.queued.load(Ordering::Relaxed) != 0 {
                        for gate in &self.gates {
                            gate.fetch_or(2, Ordering::Release);
                        }
                        if self
                            .gates
                            .iter()
                            .all(|gate| gate.fetch_or(4, Ordering::AcqRel) & 1 == 0)
                        {
                            self.queued.store(0, Ordering::Relaxed);
                        }
                    }
                    let requested = self.requests.fetch_sub(consumed, Ordering::AcqRel);
                    if requested == consumed {
                        return;
                    }
                    consumed = requested - consumed;
                }
            }
        }

        for reader_count in [1, 2] {
            let mut model = loom::model::Builder::new();
            if reader_count == 2 {
                // Three actors have a much larger schedule space. Exercise
                // two reader stripes with bounded preemption in normal CI.
                model.preemption_bound = Some(2);
                model.max_permutations = Some(20_000);
            }
            model.check(move || {
                let state = Arc::new(Maintenance {
                    gates: std::array::from_fn(|index| {
                        AtomicUsize::new(usize::from(index < reader_count))
                    }),
                    queued: AtomicUsize::new(0),
                    requests: AtomicUsize::new(0),
                });
                let readers: Vec<_> = (0..reader_count)
                    .map(|index| {
                        let reader = Arc::clone(&state);
                        loom::thread::spawn(move || {
                            reader.gates[index].fetch_sub(1, Ordering::Release);
                            fence(Ordering::Acquire);
                            if reader.queued.load(Ordering::Relaxed) != 0 {
                                reader.maintain();
                            }
                        })
                    })
                    .collect();
                let writer = Arc::clone(&state);
                let writer = loom::thread::spawn(move || {
                    writer.queued.store(1, Ordering::Relaxed);
                    writer.maintain();
                });
                for reader in readers {
                    reader.join().unwrap();
                }
                writer.join().unwrap();
                assert_eq!(state.queued.load(Ordering::Relaxed), 0);
                assert_eq!(state.requests.load(Ordering::Relaxed), 0);
            });
        }
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
        let permit = domain.domain.enter(0).unwrap();
        let initial = domain.domain.current_generation();
        let worker_registry = Arc::clone(&registry);
        let worker = std::thread::spawn(move || worker_registry.bindings.read_domain().quiesce());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while domain.domain.current_generation() == initial && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let rotated = domain.domain.current_generation() != initial;
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
        let permit = domain.domain.enter(0).unwrap();
        registry.remove::<Counted>(&first).unwrap();
        registry.remove::<Counted>(&second).unwrap();
        drop(permit);
        let [first, second] = domain
            .domain
            .quiesce_with_publication_barrier(
                |generation| domain.pending[generation.index()].lock(),
                |generation| domain.take_generation(generation),
            )
            .unwrap();
        let mut batch = first.expect("previous pending generation");
        let next = second.expect("current generation");
        assert_eq!(domain.queued.load(Ordering::Relaxed), 0);
        assert_eq!(domain.debt(), 2);
        batch.extend(next);
        assert_eq!(
            domain.debt(),
            2,
            "the emptied source batch owns no completion debt"
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        // RAII completion must work even without calling reclaim explicitly.
        drop(batch);
        assert_eq!(domain.debt(), 0);
        assert_eq!(drops.load(Ordering::Acquire), 2);
    }

    #[test]
    fn miri_last_old_reader_reclaims_while_a_later_reader_remains() {
        // Object payloads are static; keep the observation outside the arena.
        struct Counted(Arc<AtomicUsize>);
        impl ExcelHandleObject for Counted {}
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let pending = registry.new_object(Counted(Arc::clone(&drops))).unwrap();
        let token = registry.publish_pending::<Counted>(pending).unwrap().0;
        let domain = registry.bindings.read_domain();
        let old = domain.enter().unwrap();
        registry.remove::<Counted>(&token).unwrap();
        assert_eq!(drops.load(Ordering::Acquire), 0);
        let later = domain.enter().unwrap();
        drop(old);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(domain.debt(), 0);
        drop(later);
    }

    #[test]
    fn hard_debt_remover_without_a_read_permit_waits_for_grace() {
        struct Payload;
        impl ExcelHandleObject for Payload {}
        let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
        let publish = || {
            let pending = registry.new_object(Payload).unwrap();
            registry.publish_pending::<Payload>(pending).unwrap().0
        };
        let first = publish();
        let (remover, finished) =
            crate::call::with_excel_call_scope_and_state(&registry, |registry, scope| {
                let borrowed = registry.lookup_handle::<Payload>(scope, &first).unwrap();
                registry.remove::<Payload>(&first).unwrap();
                for _ in 1..HARD_DEBT_LIMIT - 1 {
                    registry.remove::<Payload>(&publish()).unwrap();
                }
                let final_token = publish();
                // The conservative stripe observation may collide. Find a fresh
                // remover whose own stripe is empty before testing blocking policy.
                let mut selected = None;
                for _ in 0..64 {
                    let removing = Arc::clone(registry);
                    let token = final_token.clone();
                    let (started_tx, started_rx) = std::sync::mpsc::channel();
                    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
                    let remover = std::thread::spawn(move || {
                        if removing
                            .bindings
                            .read_domain()
                            .domain
                            .current_thread_may_be_reading()
                        {
                            started_tx.send(false).unwrap();
                            return;
                        }
                        started_tx.send(true).unwrap();
                        removing.remove::<Payload>(&token).unwrap();
                        finished_tx.send(()).unwrap();
                    });
                    if started_rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                        selected = Some((remover, finished_rx));
                        break;
                    }
                    remover.join().unwrap();
                }
                let (remover, finished) = selected.expect("find a remover with an empty stripe");
                assert!(finished.recv_timeout(Duration::from_millis(20)).is_err());
                std::hint::black_box(&*borrowed);
                (remover, finished)
            });
        finished.recv_timeout(Duration::from_secs(30)).unwrap();
        remover.join().unwrap();
        registry.bindings.read_domain().flush_for_test();
        assert_eq!(registry.bindings.read_domain().debt(), 0);
    }
}
