//! Call-scoped handle read domain and deferred reclamation.
//!
//! Rather than acquiring a per-record reader permit on every lookup, UDF
//! calls register once with this domain for the duration of their [`CallScope`].
//! Readers enter striped drain gates partitioned by generation, avoiding
//! cache-line contention on individual binding records during concurrent
//! lookups of the same handle.
//!
//! Removed bindings enter generation-bound queues. A lazily started worker
//! reclaims even a single retirement; writers attempt idle maintenance at 32
//! outstanding records and blocking maintenance at 256 when they cannot hold
//! their own read permit. Admission backpressure bounds debt in the latter
//! case. Final seal joins maintenance and waits for destructor completion.
//! Only the current generation is open, so a reader that observed the old
//! generation before rotation cannot be admitted after the grace period starts.

#![allow(
    unsafe_code,
    reason = "owned read permits are audited temporal capabilities"
)]

use crate::{XllError, XllResult};
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    RotatingReadDomain, RotatingReadOwnedPermit, RotatingReadPermit,
};

use super::binding::BindingRecord;
use parking_lot::{Condvar, Mutex};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{Builder, JoinHandle};
use std::time::Duration;
use xlfn_kernel::published_owner::PublishedOwner;

pub(crate) struct HandleReadDomain {
    domain: RotatingReadDomain<DEFAULT_STRIPE_COUNT>,
    pending: [Mutex<Vec<PublishedOwner<BindingRecord>>>; 2],
    queued: AtomicUsize,
    debt: AtomicUsize,
    peak_debt: AtomicUsize,
    worker_started: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopping: Mutex<bool>,
    changed: Condvar,
}

pub(crate) struct HandleDomainPermit {
    pub(crate) domain: NonNull<HandleReadDomain>,
    _permit: RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>,
}

impl HandleDomainPermit {
    #[allow(
        dead_code,
        reason = "Audited capability constructor for tests and scoped readers"
    )]
    #[inline]
    pub(crate) fn witness<'scope>(&'scope self) -> HandleDomainWitness<'scope> {
        HandleDomainWitness {
            domain: self.domain,
            _marker: PhantomData,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HandleDomainWitness<'scope> {
    domain: NonNull<HandleReadDomain>,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> HandleDomainWitness<'scope> {
    /// Constructs a witness representing an active read permit for `domain` throughout `'scope`.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that an active reader permit for `domain`
    /// remains valid and will not be reclaimed for the duration of `'scope`.
    #[inline]
    pub(crate) unsafe fn new_unchecked(domain: NonNull<HandleReadDomain>) -> Self {
        Self {
            domain,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn domain(&self) -> NonNull<HandleReadDomain> {
        self.domain
    }
}

pub(crate) type HandleBindingDomainPermit<'domain> =
    RotatingReadPermit<'domain, DEFAULT_STRIPE_COUNT>;

impl HandleReadDomain {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            domain: RotatingReadDomain::new(),
            pending: [Mutex::new(Vec::new()), Mutex::new(Vec::new())],
            queued: AtomicUsize::new(0),
            debt: AtomicUsize::new(0),
            peak_debt: AtomicUsize::new(0),
            worker_started: AtomicBool::new(false),
            worker: Mutex::new(None),
            stopping: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    /// Start once, on the first publication, before any pointer is published.
    pub(crate) fn ensure_worker(self: &Arc<Self>) -> XllResult<()> {
        if self.worker_started.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut worker = self.worker.lock();
        if *self.stopping.lock() {
            return Err(XllError::Closing);
        }
        if worker.is_none() {
            let domain = Arc::clone(self);
            *worker = Some(
                Builder::new()
                    .name("xlfn-handle-reclaim".into())
                    .spawn(move || domain.run_worker())
                    .map_err(|error| XllError::Native {
                        code: error.raw_os_error().unwrap_or(0),
                        message: format!("failed to start handle maintenance: {error}"),
                    })?,
            );
            self.worker_started.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn run_worker(&self) {
        loop {
            let mut stopping = self.stopping.lock();
            while !*stopping && self.queued.load(Ordering::Acquire) == 0 {
                self.changed.wait(&mut stopping);
            }
            if *stopping {
                return;
            }
            // Coalesce small retirements, but never require another removal.
            self.changed
                .wait_for(&mut stopping, Duration::from_millis(1));
            if *stopping {
                return;
            }
            drop(stopping);
            // Rotation publishes the next generation before waiting for old
            // readers. Continuous later readers cannot starve this batch.
            self.quiesce();
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn enter(&self) -> XllResult<HandleBindingDomainPermit<'_>> {
        self.domain
            .enter_current_thread()
            .map_err(|_| XllError::Closing)
    }

    /// Enters the handle read domain and acquires an owned reader permit.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self` outlives the returned [`HandleDomainPermit`].
    /// All permits must be dropped before the domain is destroyed or moved.
    #[inline]
    pub(crate) unsafe fn enter_owned(&self) -> XllResult<HandleDomainPermit> {
        // SAFETY: guaranteed by caller's owner-lifetime contract;
        // self outlives the returned permit and will be drained before reclamation.
        unsafe {
            self.domain
                .enter_owned_current_thread()
                .map(|permit| HandleDomainPermit {
                    domain: NonNull::from(self),
                    _permit: permit,
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
    pub(crate) fn enqueue_reclaim(&self, record: PublishedOwner<BindingRecord>) {
        loop {
            let generation = self.domain.current_generation();
            let mut queue = self.pending[generation.index()].lock();
            if self.domain.current_generation() != generation {
                continue;
            }
            queue.push(record);
            let debt = self.debt.fetch_add(1, Ordering::AcqRel) + 1;
            self.peak_debt.fetch_max(debt, Ordering::Relaxed);
            self.queued.fetch_add(1, Ordering::Release);
            break;
        }
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn debt_snapshot(&self) -> (usize, usize, usize) {
        (
            self.queued.load(Ordering::Acquire),
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
        // Register wakeup under the same mutex used to park the worker.
        let stopping = self.stopping.lock();
        if !*stopping {
            self.changed.notify_one();
        }
        drop(stopping);
        if self.is_reclaiming_here() {
            return;
        }
        let debt = self.debt();
        // CallScope is !Send and all handle permits use the acquiring thread's
        // default stripe. Looking at both generations therefore excludes an
        // own permit without changing lookup/permit-entry bookkeeping.
        if debt >= HARD_DEBT_LIMIT && !self.domain.current_thread_may_be_reading() {
            self.quiesce();
        } else if debt >= SOFT_DEBT_LIMIT {
            self.maintain();
        }
    }

    fn take_generation(&self, index: usize) -> Vec<PublishedOwner<BindingRecord>> {
        let records = std::mem::take(&mut *self.pending[index].lock());
        self.queued.fetch_sub(records.len(), Ordering::AcqRel);
        records
    }

    fn reclaim(&self, records: Vec<PublishedOwner<BindingRecord>>) {
        if records.is_empty() {
            return;
        }
        let count = records.len();
        let address = std::ptr::from_ref(self).addr();
        RECLAIMING.with(|stack| stack.borrow_mut().push(address));
        let _guard = scopeguard::guard(address, |address| {
            RECLAIMING.with(|stack| assert_eq!(stack.borrow_mut().pop(), Some(address)));
        });
        // No table, queue, transition, or worker lock spans user destructors.
        drop(records);
        self.debt.fetch_sub(count, Ordering::AcqRel);
        let _stopping = self.stopping.lock();
        self.changed.notify_all();
    }

    pub(crate) fn maintain(&self) {
        if self.is_reclaiming_here() {
            return;
        }
        let records = self
            .domain
            .try_quiesce_if_idle_with_publication_barrier(
                |generation| self.pending[generation.index()].try_lock(),
                |generation| self.take_generation(generation.index()),
            )
            .and_then(Result::ok)
            .unwrap_or_default();
        self.reclaim(records);
    }

    pub(crate) fn quiesce(&self) {
        let records = self
            .domain
            .quiesce_with_publication_barrier(
                |generation| self.pending[generation.index()].lock(),
                |generation| self.take_generation(generation.index()),
            )
            .unwrap_or_default();
        self.reclaim(records);
    }

    #[cfg(test)]
    pub(crate) fn flush_for_test(&self) {
        self.quiesce();
        let mut stopping = self.stopping.lock();
        while self.debt() != 0 {
            self.changed.wait(&mut stopping);
        }
    }

    /// Final drain closes reader admission, joins maintenance, and waits for
    /// destruction already handed to another writer. A destructor releasing
    /// the last registry owner may run this from the worker itself: owning
    /// arena capabilities retain all data until that worker finishes.
    pub(crate) fn seal(&self) {
        {
            let _worker = self.worker.lock();
            *self.stopping.lock() = true;
            self.changed.notify_all();
        }
        self.domain.seal_and_wait();
        let mut records = self.take_generation(0);
        records.extend(self.take_generation(1));
        self.reclaim(records);
        let worker = self.worker.lock().take();
        if let Some(worker) = worker
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = crate::panic_boundary::contain_panic(worker.join());
        }
        if !self.is_reclaiming_here() {
            let mut stopping = self.stopping.lock();
            while self.debt() != 0 {
                self.changed.wait(&mut stopping);
            }
        }
    }
}

pub(crate) const SOFT_DEBT_LIMIT: usize = 32;
pub(crate) const HARD_DEBT_LIMIT: usize = 256;
thread_local! {
    static RECLAIMING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{ExcelHandleObject, registry::HandleRegistry};

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
