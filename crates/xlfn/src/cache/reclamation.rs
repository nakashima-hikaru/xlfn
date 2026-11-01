//! Post-grace batches and allocation recovery; raw retirement entries alone
//! grant no safe destruction API. Kernel-issued batches fix the grace domain.
use super::*;
use xlfn_kernel::rotating_read_domain::ClosedRetirement;

#[cfg(test)]
mod tests;

/// Consumes a live pin and its node's own publication history together.
/// Only this boundary can issue immediate, never-published recovery.
pub(super) unsafe fn release_node_pin<V>(node_ptr: NonNull<CacheNode<V>>) {
    // SAFETY: the caller owns the pin and retains the original domain.
    let node = unsafe { node_ptr.as_ref() };
    // SAFETY: transfer that exact live pin into final-release ownership.
    if let Some(entry) = unsafe { ReclaimEntry::release_pin(node_ptr) } {
        if !node.published && ACTIVE_CACHE_INITIALIZATION_DEPTH.get() == 0 {
            NeverPublished(entry).reclaim();
        } else {
            // SAFETY: final-pin ownership retains its original domain through
            // this maintenance tail. No caller can select another receiver.
            let domain = unsafe { entry.domain.as_ref() };
            entry.enqueue();
            reclaim_cache_entries(domain.quiesce_and_drain());
        }
    }
}

struct NeverPublished<V>(ReclaimEntry<V>);
impl<V> NeverPublished<V> {
    fn reclaim(self) {
        // SAFETY: only the final-release decision above constructs this owner,
        // after observing that the node was never published to the index.
        unsafe {
            reclaim_cache_node(self.0);
        }
    }
}

/// Consumes the final-pin retirement entry after all observations have ended.
/// The caller must establish a matching grace period, or that the node was never
/// published. Ownership of the entry alone does not establish quiescence.
unsafe fn reclaim_cache_node<V>(entry: ReclaimEntry<V>) {
    let ReclaimEntry { pointer: ptr, .. } = entry;
    // SAFETY: the caller establishes quiescence; consuming entry transfers the
    // unique retirement ownership into Box without copying the inline payload.
    let node = unsafe { Box::from_non_null(ptr) };
    // Drop in place: moving an inline, potentially large V onto the stack
    // just to catch its destructor would add copies and risk stack overflow.
    // Box's drop glue frees the node even if V's destructor unwinds. All
    // framework locks and the grace-period callback have already been left.
    if catch_no_unwind(AssertUnwindSafe(|| drop(node))).is_err() {
        let error = XllError::Panic;
        crate::diagnostics::report_no_unwind("calculation cache value final drop", &error);
    }
}

pub(super) fn reclaim_cache_entries<V>(entries: ReclaimEntries<'_, V>) {
    fn reclaim<V>(entries: PendingEntries<V>) {
        for entry in entries {
            // SAFETY: detached entries have completed their domain grace period.
            unsafe { reclaim_cache_node::<V>(entry) };
        }
    }
    if destruction_deferred() && !entries.is_empty() {
        // SAFETY: cache nodes are created only by CalculationCache with V: 'static.
        // Detached entries own the allocations independently of the cache/domain;
        // reclaim only consumes their pointers, never the old domain pointer.
        unsafe { deferred_drop::defer(entries.entries, reclaim::<V>) };
        return;
    }
    for entry in entries.entries {
        // SAFETY: [TR-RECLAIM-1] entry.pointer points to an allocated CacheNode<V> whose grace period has ended.
        unsafe {
            reclaim_cache_node::<V>(entry);
        }
    }
}

/// A batch detached only after a same-domain grace-period certificate.
/// The owner borrow keeps the allocation's domain alive through reclamation.
pub(super) struct ReclaimEntries<'domain, V> {
    _domain: &'domain CacheLookupDomain<V>,
    entries: PendingEntries<V>,
}

impl<'domain, V> ReclaimEntries<'domain, V> {
    fn account(domain: &'domain CacheLookupDomain<V>, entries: PendingEntries<V>) -> Self {
        // Keep detached entries counted until their weight accounting completes.
        // A concurrent empty-queue observer may reset saturation only after
        // this final pending_nodes subtraction makes the whole domain empty.
        if !domain.pending_weight_saturated.load(Ordering::Relaxed) {
            let weight = entries
                .iter()
                .fold(0_u64, |sum, entry| sum.saturating_add(entry.weight));
            domain
                .pending_weight
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current.checked_sub(weight)
                })
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        }
        domain
            .pending_nodes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(entries.len())
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        ReclaimEntries {
            _domain: domain,
            entries,
        }
    }

    fn new(domain: &'domain CacheLookupDomain<V>) -> Self {
        Self {
            _domain: domain,
            entries: PendingEntries::new(),
        }
    }
}
impl<V> std::ops::Deref for ReclaimEntries<'_, V> {
    type Target = [ReclaimEntry<V>];
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

// Called only while one receiver operation consumes its own kernel batches.
// This helper moves storage, never proof-bearing batches from another owner.
fn append_records<V>(entries: &mut PendingEntries<V>, mut next: PendingEntries<V>) {
    if entries.is_empty() {
        *entries = next;
    } else {
        entries.append(&mut next);
    }
}

/// Terminal admission belongs to this cache domain. Closing and detaching stay
/// separate because cache destruction must release index residency in between.
/// The caller cannot substitute another domain when detaching the queues.
pub(super) struct ClosedCacheDomain<'domain, V> {
    domain: &'domain CacheLookupDomain<V>,
    closed: ClosedRetirement<'domain, DEFAULT_STRIPE_COUNT, PendingEntries<V>>,
}

impl<'domain, V> ClosedCacheDomain<'domain, V> {
    pub(super) fn drain(self) -> ReclaimEntries<'domain, V> {
        let Self { domain, closed } = self;
        let [first, second] = closed.take_queues();
        let mut entries = first.into_inner();
        append_records(&mut entries, second.into_inner());
        ReclaimEntries {
            _domain: domain,
            entries,
        }
    }
}

impl<V> CacheLookupDomain<V> {
    pub(super) fn quiesce_and_drain(&self) -> ReclaimEntries<'_, V> {
        // Initializers/unwind cleanup must not wait for a caller blocked on
        // this operation. Advance nonblocking quiescence in those cases. A
        // synchronous writer also must not await its own lookup admission.
        if ACTIVE_CACHE_INITIALIZATION_DEPTH.get() != 0 || std::thread::panicking() {
            return self.try_quiesce_and_drain();
        }
        if self.current_thread_is_reading() {
            return ReclaimEntries::new(self);
        }
        #[cfg(any(test, feature = "bench-internals"))]
        let start = Instant::now();
        let mut records = PendingEntries::new();
        for batch in self
            .domain
            .quiesce()
            .unwrap_or_default()
            .into_iter()
            .flatten()
        {
            append_records(&mut records, batch.into_inner());
        }
        let entries = ReclaimEntries::account(self, records);
        self.clear_saturated_weight_if_empty();
        #[cfg(any(test, feature = "bench-internals"))]
        self.record_batch(&entries, start);
        entries
    }

    pub(super) fn try_quiesce_and_drain(&self) -> ReclaimEntries<'_, V> {
        if self.pending_nodes.load(Ordering::Relaxed) == 0 {
            return ReclaimEntries::new(self);
        }
        // Every poll participates in one request handoff. A departing reader
        // racing a driver's failed idle check forces that driver to poll again.
        let previous = self
            .maintenance_requests
            .try_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                requests.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        if previous != 0 {
            return ReclaimEntries::new(self);
        }
        #[cfg(any(test, feature = "bench-internals"))]
        let start = Instant::now();
        let mut records = PendingEntries::new();
        let mut consumed = 1;
        loop {
            while self.pending_nodes.load(Ordering::Relaxed) != 0 {
                let Some(Ok(batch)) = self.domain.poll_quiesce() else {
                    break;
                };
                let batch = ReclaimEntries::account(self, batch.into_inner());
                append_records(&mut records, batch.entries);
            }
            let requested = self
                .maintenance_requests
                .try_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                    requests.checked_sub(consumed)
                })
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            if requested == consumed {
                break;
            }
            consumed = requested - consumed;
        }
        let entries = ReclaimEntries {
            _domain: self,
            entries: records,
        };
        self.clear_saturated_weight_if_empty();
        #[cfg(any(test, feature = "bench-internals"))]
        self.record_batch(&entries, start);
        entries
    }

    #[cfg(test)]
    fn poll_and_drain(
        &self,
    ) -> Option<Result<ReclaimEntries<'_, V>, xlfn_kernel::rotating_read_domain::DomainClosed>>
    {
        self.domain
            .poll_quiesce()
            .map(|result| result.map(|batch| ReclaimEntries::account(self, batch.into_inner())))
    }

    #[cfg(test)]
    pub(super) fn try_idle_and_drain(
        &self,
    ) -> Option<Result<ReclaimEntries<'_, V>, xlfn_kernel::rotating_read_domain::DomainClosed>>
    {
        self.domain
            .try_quiesce_if_idle()
            .map(|result| result.map(|batch| ReclaimEntries::account(self, batch.into_inner())))
    }

    pub(super) fn seal(&self) -> ClosedCacheDomain<'_, V> {
        ClosedCacheDomain {
            domain: self,
            closed: self.domain.seal_and_wait(),
        }
    }
}
