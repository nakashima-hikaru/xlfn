//! Post-grace batches and allocation recovery; raw retirement entries alone
//! grant no safe destruction API. Kernel-issued batches fix the grace domain.
use super::*;

pub(super) fn reclaim_unpublished<V>(entry: ReclaimEntry<V>) {
    // SAFETY: final-pin ownership retains this allocation; inspect its actual
    // publication history rather than trusting a caller-provided boolean.
    if unsafe { entry.pointer.as_ref() }.published {
        xlfn_kernel::invariant::fail_stop();
    }
    // SAFETY: this allocation was never observable through the resident index.
    unsafe {
        reclaim_cache_node(entry);
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
    domain: &'domain CacheLookupDomain<V>,
    entries: PendingEntries<V>,
}

impl<'domain, V> ReclaimEntries<'domain, V> {
    #[cfg(test)]
    pub(super) fn belongs_to(&self, domain: &CacheLookupDomain<V>) -> bool {
        std::ptr::eq(self.domain, domain)
    }
    pub(super) fn from_closed(
        domain: &'domain CacheLookupDomain<V>,
        closed: ClosedRetirement<'_, DEFAULT_STRIPE_COUNT, PendingEntries<V>>,
    ) -> Self {
        if !closed.belongs_to(&domain.domain) {
            xlfn_kernel::invariant::fail_stop();
        }
        let batches = closed.take_queues().map(|batch| Self {
            domain,
            entries: batch.into_inner(),
        });
        merge_reclaims(domain, batches)
    }

    pub(super) fn from_disposable(
        domain: &'domain CacheLookupDomain<V>,
        generation: DisposableBatch<'_, DEFAULT_STRIPE_COUNT, PendingEntries<V>>,
    ) -> ReclaimEntries<'domain, V> {
        if !generation.belongs_to(&domain.domain) {
            xlfn_kernel::invariant::fail_stop();
        }
        let entries = generation.into_inner();
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
        ReclaimEntries { domain, entries }
    }

    pub(super) fn new(domain: &'domain CacheLookupDomain<V>) -> Self {
        Self {
            domain,
            entries: PendingEntries::new(),
        }
    }
    pub(super) fn extend(&mut self, mut other: Self) {
        crate::retirement_queue::append_owned_batch!(
            std::ptr::from_ref(self.domain), std::ptr::from_ref(other.domain);
            xlfn_kernel::invariant::fail_stop(),
            self.entries.append(&mut other.entries)
        );
    }
}
impl<V> std::ops::Deref for ReclaimEntries<'_, V> {
    type Target = [ReclaimEntry<V>];
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

pub(super) fn merge_reclaims<'domain, V>(
    domain: &'domain CacheLookupDomain<V>,
    batches: impl IntoIterator<Item = ReclaimEntries<'domain, V>>,
) -> ReclaimEntries<'domain, V> {
    let entries = batches
        .into_iter()
        .filter(|batch| !batch.is_empty())
        .reduce(|mut entries, next| {
            entries.extend(next);
            entries
        })
        .unwrap_or_else(|| ReclaimEntries::new(domain));
    crate::retirement_queue::append_owned_batch!(
        std::ptr::from_ref(entries.domain), std::ptr::from_ref(domain);
        xlfn_kernel::invariant::fail_stop(), entries
    )
}
