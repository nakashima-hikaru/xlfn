//! Owned accounting shared by channels without retaining a runtime capability.

use super::topic::{RtdCapacity, RtdLimits};
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::num::NonZeroUsize;
use triomphe::Arc;

/// How accepted values wait for publication to Excel's RTD storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RtdDelivery {
    /// Retain values in arrival order, up to this per-subscription queue length.
    /// Excel may still coalesce intermediate values during refresh.
    Fifo(NonZeroUsize),
    /// Replace the unconsumed value with the latest accepted value.
    /// An already publishing value is not retracted. Failed admission preserves
    /// the previous value and returns the rejected candidate to the caller.
    Latest,
}

/// Aggregate admission limits shared by all subscriptions of one source.
///
/// Buffered limits include values being published. The byte charge is the
/// stored value representation plus owned UTF-8 string bytes, excluding
/// allocator overhead, queue spare capacity, Excel storage and upstream I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RtdSourceLimits {
    subscriptions: RtdCapacity,
    values: RtdCapacity,
    bytes: RtdCapacity,
}

impl RtdSourceLimits {
    /// Default to 4,096 subscriptions, 4,096 buffered values and 64 MiB.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            subscriptions: RtdCapacity::disabled_if_zero(4096),
            values: RtdCapacity::disabled_if_zero(4096),
            bytes: RtdCapacity::disabled_if_zero(64 * 1024 * 1024),
        }
    }
    /// Sets the number of subscriptions retained until their shutdown barrier.
    #[must_use]
    pub const fn with_max_subscriptions(mut self, limit: RtdCapacity) -> Self {
        self.subscriptions = limit;
        self
    }
    /// Sets the aggregate count of queued and currently publishing values.
    #[must_use]
    pub const fn with_max_buffered_values(mut self, limit: RtdCapacity) -> Self {
        self.values = limit;
        self
    }
    /// Sets the aggregate retained-value byte charge.
    #[must_use]
    pub const fn with_max_buffered_bytes(mut self, limit: RtdCapacity) -> Self {
        self.bytes = limit;
        self
    }
    /// Returns the subscription bound.
    #[must_use]
    pub const fn max_subscriptions(self) -> RtdCapacity {
        self.subscriptions
    }
    /// Returns the queued and publishing value bound.
    #[must_use]
    pub const fn max_buffered_values(self) -> RtdCapacity {
        self.values
    }
    /// Returns the retained-value byte bound.
    #[must_use]
    pub const fn max_buffered_bytes(self) -> RtdCapacity {
        self.bytes
    }
}

impl Default for RtdSourceLimits {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
struct Usage {
    subscriptions: usize,
    values: usize,
    bytes: usize,
    workers: usize,
}

struct State {
    limit: Usage,
    used: Usage,
}

pub(super) struct ChannelBudget {
    state: Mutex<State>,
}

impl ChannelBudget {
    pub(super) fn source(limits: RtdSourceLimits) -> Self {
        Self::new(Usage {
            subscriptions: limits.subscriptions.get(),
            values: limits.values.get(),
            bytes: limits.bytes.get(),
            workers: usize::MAX,
        })
    }

    pub(super) fn runtime(limits: RtdLimits) -> Self {
        Self::new(Usage {
            subscriptions: usize::MAX,
            values: limits.max_buffered_values().get(),
            bytes: limits.max_buffered_bytes().get(),
            workers: limits.max_workers().get(),
        })
    }

    fn new(limit: Usage) -> Self {
        Self {
            state: Mutex::new(State {
                limit,
                used: Usage::default(),
            }),
        }
    }

    pub(super) fn configure_source(&self, limits: RtdSourceLimits) {
        let mut state = self.state.lock();
        state.limit.subscriptions = limits.subscriptions.get();
        state.limit.values = limits.values.get();
        state.limit.bytes = limits.bytes.get();
    }

    pub(super) fn configure_worker_limit(&self, limit: NonZeroUsize) {
        self.state.lock().limit.workers = limit.get();
    }

    pub(super) fn worker_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.state.lock().limit.workers)
            .expect("source producer limits are nonzero")
    }

    fn acquire(&self, amount: Usage) -> bool {
        let mut state = self.state.lock();
        let add = |used: usize, amount: usize, limit: usize| {
            // Lowering one limit must not stop unrelated resources already
            // admitted under it (for example, existing subscriptions publishing).
            if amount == 0 {
                Some(used)
            } else {
                used.checked_add(amount).filter(|next| *next <= limit)
            }
        };
        let Some(subscriptions) = add(
            state.used.subscriptions,
            amount.subscriptions,
            state.limit.subscriptions,
        ) else {
            return false;
        };
        let Some(values) = add(state.used.values, amount.values, state.limit.values) else {
            return false;
        };
        let Some(bytes) = add(state.used.bytes, amount.bytes, state.limit.bytes) else {
            return false;
        };
        let Some(workers) = add(state.used.workers, amount.workers, state.limit.workers) else {
            return false;
        };
        state.used = Usage {
            subscriptions,
            values,
            bytes,
            workers,
        };
        true
    }

    #[inline]
    fn acquire_buffered(&self, values: usize, bytes: usize) -> bool {
        let mut state = self.state.lock();
        let Some(next_values) = state.used.values.checked_add(values) else {
            return false;
        };
        let Some(next_bytes) = state.used.bytes.checked_add(bytes) else {
            return false;
        };
        if (values != 0 && next_values > state.limit.values)
            || (bytes != 0 && next_bytes > state.limit.bytes)
        {
            return false;
        }
        state.used.values = next_values;
        state.used.bytes = next_bytes;
        true
    }

    #[inline]
    fn release_buffered(&self, values: usize, bytes: usize) {
        let mut state = self.state.lock();
        state.used.values -= values;
        state.used.bytes -= bytes;
    }

    fn release(&self, amount: Usage) {
        let mut state = self.state.lock();
        state.used.subscriptions -= amount.subscriptions;
        state.used.values -= amount.values;
        state.used.bytes -= amount.bytes;
        state.used.workers -= amount.workers;
    }

    pub(super) fn try_subscription(this: &Arc<Self>) -> XllResult<SubscriptionPermit> {
        let amount = Usage {
            subscriptions: 1,
            ..Usage::default()
        };
        if !this.acquire(amount) {
            return Err(XllError::Overloaded);
        }
        Ok(SubscriptionPermit(Arc::clone(this)))
    }

    pub(super) fn try_worker(this: &Arc<Self>) -> XllResult<WorkerPermit> {
        let amount = Usage {
            workers: 1,
            ..Usage::default()
        };
        if !this.acquire(amount) {
            return Err(XllError::Overloaded);
        }
        Ok(WorkerPermit(Arc::clone(this)))
    }
}

pub(super) struct SubscriptionPermit(Arc<ChannelBudget>);
impl Drop for SubscriptionPermit {
    fn drop(&mut self) {
        self.0.release(Usage {
            subscriptions: 1,
            ..Usage::default()
        });
    }
}

pub(super) struct WorkerPermit(Arc<ChannelBudget>);
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.0.release(Usage {
            workers: 1,
            ..Usage::default()
        });
    }
}

pub(super) struct BudgetPair {
    source: Arc<ChannelBudget>,
    runtime: Arc<ChannelBudget>,
}

impl BudgetPair {
    pub(super) fn new(source: Arc<ChannelBudget>, runtime: Arc<ChannelBudget>) -> Self {
        Self { source, runtime }
    }

    #[inline]
    fn acquire(&self, values: usize, bytes: usize) -> bool {
        if !self.source.acquire_buffered(values, bytes) {
            return false;
        }
        if !self.runtime.acquire_buffered(values, bytes) {
            self.source.release_buffered(values, bytes);
            return false;
        }
        true
    }

    #[inline]
    fn release(&self, values: usize, bytes: usize) {
        self.source.release_buffered(values, bytes);
        self.runtime.release_buffered(values, bytes);
    }

    #[cfg(test)]
    pub(super) fn try_value(&self, bytes: usize) -> Option<BufferPermit<'_>> {
        self.acquire(1, bytes).then(|| BufferPermit {
            budgets: self,
            values: 1,
            bytes,
        })
    }
}

// The channel owns both budgets outside its state mutex. Queue accounting is
// count-only, so splitting a batch borrows those budgets without cloning Arc.
// Channel::drop drains this ledger before its budget owners are reclaimed.
#[derive(Default)]
pub(super) struct BufferLedger {
    values: usize,
    bytes: usize,
}

impl BufferLedger {
    pub(super) const fn bytes(&self) -> usize {
        self.bytes
    }

    #[inline]
    pub(super) fn try_push(&mut self, budgets: &BudgetPair, bytes: usize) -> bool {
        if !budgets.acquire(1, bytes) {
            return false;
        }
        self.values += 1;
        self.bytes += bytes;
        true
    }

    pub(super) fn split<'a>(
        &mut self,
        budgets: &'a BudgetPair,
        values: usize,
        bytes: usize,
    ) -> Option<BufferPermit<'a>> {
        if values == 0 {
            debug_assert_eq!(bytes, 0);
            return None;
        }
        assert!(values <= self.values && bytes <= self.bytes);
        self.values -= values;
        self.bytes -= bytes;
        Some(BufferPermit {
            budgets,
            values,
            bytes,
        })
    }

    pub(super) fn take_all<'a>(&mut self, budgets: &'a BudgetPair) -> Option<BufferPermit<'a>> {
        self.split(budgets, self.values, self.bytes)
    }

    pub(super) fn resize(&mut self, budgets: &BudgetPair, bytes: usize) -> bool {
        if !resize_charge(budgets, self.bytes, bytes) {
            return false;
        }
        self.bytes = bytes;
        true
    }
}

fn resize_charge(budgets: &BudgetPair, current: usize, requested: usize) -> bool {
    if requested > current {
        budgets.acquire(0, requested - current)
    } else {
        if requested < current {
            budgets.release(0, current - requested);
        }
        true
    }
}

// A batch can outlive the queue lock, but never the channel that owns its
// budgets. It returns capacity after the batch values have been destroyed.
pub(super) struct BufferPermit<'a> {
    budgets: &'a BudgetPair,
    values: usize,
    bytes: usize,
}

#[cfg(test)]
impl BufferPermit<'_> {
    fn resize(&mut self, bytes: usize) -> bool {
        if !resize_charge(self.budgets, self.bytes, bytes) {
            return false;
        }
        self.bytes = bytes;
        true
    }
}

impl Drop for BufferPermit<'_> {
    fn drop(&mut self) {
        if self.values != 0 || self.bytes != 0 {
            self.budgets.release(self.values, self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded(n: usize) -> RtdCapacity {
        RtdCapacity::disabled_if_zero(n)
    }

    #[test]
    fn miri_aggregate_batch_transfer_keeps_capacity_until_each_owner_drops() {
        let source = Arc::new(ChannelBudget::source(
            RtdSourceLimits::standard()
                .with_max_buffered_values(bounded(3))
                .with_max_buffered_bytes(bounded(6)),
        ));
        let runtime = Arc::new(ChannelBudget::runtime(RtdLimits::standard()));
        let pair = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let mut queue = BufferLedger::default();
        assert!(queue.try_push(&pair, 1));
        assert!(queue.try_push(&pair, 2));
        assert!(queue.try_push(&pair, 3));
        let batch = queue.split(&pair, 2, 3).unwrap();
        assert!(!queue.try_push(&pair, 1));
        assert!(pair.try_value(1).is_none());
        assert_eq!(source.state.lock().used.values, 3);
        assert_eq!(source.state.lock().used.bytes, 6);
        // Discarding the remaining queue cannot release in-flight capacity.
        drop(queue.take_all(&pair));
        assert_eq!(runtime.state.lock().used.values, 2);
        assert_eq!(runtime.state.lock().used.bytes, 3);
        assert!(queue.try_push(&pair, 3));
        drop(queue.take_all(&pair));
        assert_eq!(runtime.state.lock().used.values, 2);
        drop(batch);
        assert_eq!(source.state.lock().used, Usage::default());
        // The pair owns accounting even after the original source/runtime owners drop.
        let mut queue = BufferLedger::default();
        assert!(queue.try_push(&pair, 6));
        let batch = queue.take_all(&pair);
        drop(queue.take_all(&pair));
        drop(source);
        drop(runtime);
        drop(batch);
    }

    #[test]
    fn aggregate_batch_unwind_releases_only_the_transferred_charge() {
        let source = Arc::new(ChannelBudget::source(RtdSourceLimits::standard()));
        let runtime = Arc::new(ChannelBudget::runtime(
            RtdLimits::standard().with_max_buffered_values(bounded(2)),
        ));
        let pair = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let mut queue = BufferLedger::default();
        assert!(queue.try_push(&pair, 10));
        assert!(queue.try_push(&pair, 20));
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            let _batch = queue.split(&pair, 1, 10);
            assert!(pair.try_value(1).is_none());
            panic!("injected publication unwind after batch transfer");
        }));
        assert!(result.is_err());
        assert_eq!(runtime.state.lock().used.values, 1);
        assert_eq!(runtime.state.lock().used.bytes, 20);
        drop(pair.try_value(1).unwrap());
        drop(queue.take_all(&pair));
        assert_eq!(source.state.lock().used, Usage::default());
        assert_eq!(runtime.state.lock().used, Usage::default());
    }

    #[test]
    fn miri_budget_rolls_back_source_when_runtime_rejects_and_reuses_release() {
        let source = Arc::new(ChannelBudget::source(RtdSourceLimits::standard()));
        let runtime = Arc::new(ChannelBudget::runtime(
            RtdLimits::standard()
                .with_max_buffered_values(bounded(1))
                .with_max_buffered_bytes(bounded(20)),
        ));
        let pair = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let permit = pair.try_value(10).unwrap();
        assert!(pair.try_value(1).is_none());
        assert_eq!(source.state.lock().used.values, 1);
        assert_eq!(source.state.lock().used.bytes, 10);
        drop(permit);
        assert!(pair.try_value(21).is_none());
        assert_eq!(source.state.lock().used, Usage::default());
        drop(pair.try_value(20).unwrap());
        assert_eq!(runtime.state.lock().used, Usage::default());
    }

    #[test]
    fn replacement_reserves_only_growth_and_rolls_back_rejected_growth() {
        let source = Arc::new(ChannelBudget::source(
            RtdSourceLimits::standard()
                .with_max_buffered_values(bounded(1))
                .with_max_buffered_bytes(bounded(20)),
        ));
        let runtime = Arc::new(ChannelBudget::runtime(
            RtdLimits::standard().with_max_buffered_bytes(bounded(12)),
        ));
        let pair = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let mut permit = pair.try_value(10).unwrap();
        assert!(!permit.resize(13));
        assert_eq!(source.state.lock().used.bytes, 10);
        assert_eq!(runtime.state.lock().used.bytes, 10);
        assert!(permit.resize(12));
        assert_eq!(source.state.lock().used.values, 1);
        assert!(permit.resize(2));
        assert_eq!(runtime.state.lock().used.bytes, 2);
        drop(permit);
        assert_eq!(source.state.lock().used, Usage::default());
    }

    #[test]
    fn source_and_runtime_limits_are_independent_and_shared() {
        let source = Arc::new(ChannelBudget::source(
            RtdSourceLimits::standard().with_max_buffered_values(bounded(1)),
        ));
        let second_source = Arc::new(ChannelBudget::source(RtdSourceLimits::standard()));
        let runtime = Arc::new(ChannelBudget::runtime(
            RtdLimits::standard().with_max_buffered_values(bounded(2)),
        ));
        let first = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let same_source = BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime));
        let other_source = BudgetPair::new(second_source, Arc::clone(&runtime));
        let one = first.try_value(1).unwrap();
        assert!(same_source.try_value(1).is_none());
        let two = other_source.try_value(1).unwrap();
        assert!(other_source.try_value(1).is_none());
        drop(one);
        drop(same_source.try_value(1).unwrap());
        drop(two);
        assert_eq!(runtime.state.lock().used, Usage::default());
    }

    #[test]
    fn miri_worker_and_subscription_permits_outlive_their_original_owners() {
        let source = Arc::new(ChannelBudget::source(
            RtdSourceLimits::standard().with_max_subscriptions(bounded(1)),
        ));
        let runtime = Arc::new(ChannelBudget::runtime(
            RtdLimits::standard().with_max_workers(bounded(1)),
        ));
        let subscription = ChannelBudget::try_subscription(&source).unwrap();
        let worker = ChannelBudget::try_worker(&runtime).unwrap();
        assert!(ChannelBudget::try_subscription(&source).is_err());
        assert!(ChannelBudget::try_worker(&runtime).is_err());
        drop(source);
        drop(runtime);
        drop(subscription);
        drop(worker);
    }

    #[test]
    fn reconfiguring_source_preserves_existing_reservations() {
        let source = Arc::new(ChannelBudget::source(RtdSourceLimits::standard()));
        let subscription = ChannelBudget::try_subscription(&source).unwrap();
        let second = ChannelBudget::try_subscription(&source).unwrap();
        source.configure_source(RtdSourceLimits::standard().with_max_subscriptions(bounded(1)));
        assert!(ChannelBudget::try_subscription(&source).is_err());
        let runtime = Arc::new(ChannelBudget::runtime(RtdLimits::standard()));
        let pair = BudgetPair::new(Arc::clone(&source), runtime);
        drop(pair.try_value(10).unwrap());
        drop(second);
        drop(subscription);
        drop(ChannelBudget::try_subscription(&source).unwrap());
    }
}
