//! Source-local, movable subscription values. No callback runs under this lock.

use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SlotKey {
    index: usize,
    generation: NonZeroU64,
}

enum State<T> {
    Vacant { next: Option<usize> },
    Reserved,
    Occupied(T),
}

struct Slot<T> {
    generation: NonZeroU64,
    state: State<T>,
}

struct Slots<T> {
    entries: Vec<Slot<T>>,
    free: Option<usize>,
    live: usize,
}

pub(super) struct SubscriptionStorage<T> {
    slots: Mutex<Slots<T>>,
}

impl<T> SubscriptionStorage<T> {
    pub(super) const fn new() -> Self {
        Self {
            slots: Mutex::new(Slots {
                entries: Vec::new(),
                free: None,
                live: 0,
            }),
        }
    }

    /// Allocate before handing a sink to application code. Once reserved,
    /// publication, checkout and release do not allocate.
    pub(super) fn reserve(&self) -> XllResult<ReservedSlot<'_, T>> {
        let mut slots = self.slots.lock();
        let key = if let Some(index) = slots.free {
            let slot = &mut slots.entries[index];
            let State::Vacant { next } = slot.state else {
                xlfn_kernel::invariant::fail_stop();
            };
            // Exhausted slots are never linked back into the free list.
            slot.generation = slot
                .generation
                .checked_add(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            slot.state = State::Reserved;
            let key = SlotKey {
                index,
                generation: slot.generation,
            };
            slots.free = next;
            key
        } else {
            slots
                .entries
                .try_reserve(1)
                .map_err(|_| XllError::Overloaded)?;
            let key = SlotKey {
                index: slots.entries.len(),
                generation: NonZeroU64::MIN,
            };
            slots.entries.push(Slot {
                generation: key.generation,
                state: State::Reserved,
            });
            key
        };
        slots.live += 1;
        Ok(ReservedSlot {
            storage: self,
            key,
            armed: true,
        })
    }

    /// Detach before calling application code. The reservation prevents slot
    /// reuse while cancellation or the disconnection barrier is in progress.
    pub(super) fn checkout(&self, key: SlotKey) -> XllResult<CheckedOut<'_, T>> {
        let mut slots = self.slots.lock();
        let slot = slots
            .entries
            .get_mut(key.index)
            .ok_or(XllError::StaleHandle)?;
        if slot.generation != key.generation || !matches!(slot.state, State::Occupied(_)) {
            return Err(XllError::StaleHandle);
        }
        let State::Occupied(value) = std::mem::replace(&mut slot.state, State::Reserved) else {
            xlfn_kernel::invariant::fail_stop();
        };
        Ok(CheckedOut {
            storage: self,
            key,
            value: Some(value),
        })
    }

    fn publish(&self, key: SlotKey, value: T) {
        let mut slots = self.slots.lock();
        let slot = &mut slots.entries[key.index];
        if slot.generation != key.generation || !matches!(slot.state, State::Reserved) {
            xlfn_kernel::invariant::fail_stop();
        }
        slot.state = State::Occupied(value);
    }

    fn release(&self, key: SlotKey) {
        let mut slots = self.slots.lock();
        let next = slots.free;
        let slot = &mut slots.entries[key.index];
        if slot.generation != key.generation || !matches!(slot.state, State::Reserved) {
            xlfn_kernel::invariant::fail_stop();
        }
        // Never wrap an identity. A saturated slot becomes a permanent tombstone.
        if key.generation.get() == u64::MAX {
            slot.state = State::Vacant { next: None };
        } else {
            slot.state = State::Vacant { next };
            slots.free = Some(key.index);
        }
        slots.live -= 1;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.slots.lock().live == 0
    }
}

/// Failed or panicking subscribe calls return their unpopulated reservation.
pub(super) struct ReservedSlot<'a, T> {
    storage: &'a SubscriptionStorage<T>,
    key: SlotKey,
    armed: bool,
}

impl<T> ReservedSlot<'_, T> {
    pub(super) fn publish(mut self, value: T) -> SlotKey {
        self.storage.publish(self.key, value);
        self.armed = false;
        self.key
    }
}

impl<T> Drop for ReservedSlot<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            self.storage.release(self.key);
        }
    }
}

pub(super) struct CheckedOut<'a, T> {
    storage: &'a SubscriptionStorage<T>,
    key: SlotKey,
    value: Option<T>,
}

impl<T> CheckedOut<'_, T> {
    pub(super) fn value(&self) -> &T {
        self.value.as_ref().expect("checked out subscription")
    }

    pub(super) fn take(&mut self) -> T {
        self.value.take().expect("checked out subscription")
    }
}

impl<T> Drop for CheckedOut<'_, T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            // Cancellation may unwind; restore its unique owner for the
            // subsequent disconnect pass rather than merely dropping it.
            self.storage.publish(self.key, value);
        } else {
            // A consuming disconnect has returned or unwound, completing
            // its required sink-user barrier before this slot is reusable.
            self.storage.release(self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panic_boundary::catch_no_unwind;
    use crate::subscription::source::{
        ErasedRtdSource, RtdSink, RtdSource, RtdSubscription, SourceAdapter, SourceSubscription,
    };
    use crate::subscription::topic::RtdTopic;
    use std::cell::Cell;
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Weak};

    fn take<T>(storage: &SubscriptionStorage<T>, key: SlotKey) -> T {
        storage.checkout(key).unwrap().take()
    }

    #[test]
    fn reservation_error_and_unwind_return_unpopulated_slots() {
        let storage = SubscriptionStorage::<Cell<usize>>::new();
        let mut failed_key = None;
        let result: XllResult<()> = (|| {
            let reservation = storage.reserve()?;
            failed_key = Some(reservation.key);
            assert!(!storage.is_empty());
            assert!(matches!(
                storage.checkout(reservation.key),
                Err(XllError::StaleHandle)
            ));
            Err(XllError::Overloaded)
        })();
        assert!(matches!(result, Err(XllError::Overloaded)));
        assert!(storage.is_empty());

        let mut panicked_key = None;
        let result = catch_no_unwind(AssertUnwindSafe(|| {
            let reservation = storage.reserve().unwrap();
            panicked_key = Some(reservation.key);
            panic!("injected subscribe panic before publication");
        }));
        assert!(result.is_err());
        assert!(storage.is_empty());

        let failed_key = failed_key.unwrap();
        let panicked_key = panicked_key.unwrap();
        assert_eq!(failed_key.index, panicked_key.index);
        assert_eq!(
            panicked_key.generation.get(),
            failed_key.generation.get() + 1
        );
        let key = storage.reserve().unwrap().publish(Cell::new(41));
        assert_eq!(key.index, failed_key.index);
        assert_eq!(key.generation.get(), panicked_key.generation.get() + 1);
        assert_eq!(take(&storage, key).get(), 41);
        assert!(storage.is_empty());
    }

    #[test]
    fn checked_out_value_survives_vector_growth_without_slot_reuse() {
        let storage = SubscriptionStorage::new();
        let key = storage.reserve().unwrap().publish(Cell::new(10));
        let original_capacity = storage.slots.lock().entries.capacity();
        let checked_out = storage.checkout(key).unwrap();
        assert!(matches!(storage.checkout(key), Err(XllError::StaleHandle)));

        let mut later_keys = Vec::new();
        for value in 0..=original_capacity {
            let later = storage.reserve().unwrap().publish(Cell::new(value));
            assert_ne!(later.index, key.index);
            later_keys.push(later);
        }
        assert!(storage.slots.lock().entries.capacity() > original_capacity);
        checked_out.value().set(23);
        drop(checked_out);
        assert_eq!(take(&storage, key).get(), 23);
        for (value, later) in later_keys.into_iter().enumerate() {
            assert_eq!(take(&storage, later).get(), value);
        }
        assert!(storage.is_empty());
    }

    #[derive(Clone, Copy)]
    enum Failure {
        None,
        CancelPanic,
        DisconnectError,
        DisconnectPanic,
    }

    struct SubscriptionProbe {
        storage: Weak<SubscriptionStorage<Self>>,
        key: SlotKey,
        other: Option<SlotKey>,
        cancelled: Cell<bool>,
        drops: Arc<AtomicUsize>,
        failure: Failure,
    }

    impl SubscriptionProbe {
        fn insert(
            storage: &Arc<SubscriptionStorage<Self>>,
            drops: &Arc<AtomicUsize>,
            other: Option<SlotKey>,
            failure: Failure,
        ) -> SlotKey {
            let reservation = storage.reserve().unwrap();
            let value = Self {
                storage: Arc::downgrade(storage),
                key: reservation.key,
                other,
                cancelled: Cell::new(false),
                drops: Arc::clone(drops),
                failure,
            };
            reservation.publish(value)
        }

        fn touch_other_slot(&self) {
            let storage = self.storage.upgrade().unwrap();
            let slots = storage
                .slots
                .try_lock()
                .expect("application callback or destructor ran under storage lock");
            assert!(matches!(
                slots.entries[self.key.index].state,
                State::Reserved
            ));
            drop(slots);
            if let Some(other) = self.other {
                let checked_out = storage.checkout(other).unwrap();
                checked_out.value().cancelled.set(true);
            }
        }
    }

    // SAFETY: the fixture never receives or retains a sink and starts no worker.
    unsafe impl RtdSubscription for SubscriptionProbe {
        fn request_cancel(&self) {
            self.touch_other_slot();
            self.cancelled.set(true);
            if matches!(self.failure, Failure::CancelPanic) {
                panic!("injected cancellation panic");
            }
        }

        fn disconnect_and_wait(self) -> XllResult<()> {
            self.touch_other_slot();
            match self.failure {
                Failure::DisconnectError => Err(XllError::Overloaded),
                Failure::DisconnectPanic => panic!("injected disconnect panic"),
                Failure::None | Failure::CancelPanic => Ok(()),
            }
        }
    }

    impl Drop for SubscriptionProbe {
        fn drop(&mut self) {
            self.touch_other_slot();
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn cancellation_panic_restores_unique_owner_and_allows_reentrant_other_slot() {
        let storage = Arc::new(SubscriptionStorage::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let other = SubscriptionProbe::insert(&storage, &drops, None, Failure::None);
        let key = SubscriptionProbe::insert(&storage, &drops, Some(other), Failure::CancelPanic);

        let result = catch_no_unwind(AssertUnwindSafe(|| {
            let checked_out = storage.checkout(key).unwrap();
            checked_out.value().request_cancel();
        }));
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(storage.checkout(key).unwrap().value().cancelled.get());
        assert!(storage.checkout(other).unwrap().value().cancelled.get());
        assert!(!storage.is_empty());

        let mut checked_out = storage.checkout(key).unwrap();
        checked_out.take().disconnect_and_wait().unwrap();
        drop(checked_out);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let mut checked_out = storage.checkout(other).unwrap();
        checked_out.take().disconnect_and_wait().unwrap();
        drop(checked_out);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(storage.is_empty());
    }

    #[test]
    fn disconnect_success_error_and_panic_release_slot_after_unlocked_value_drop() {
        for failure in [
            Failure::None,
            Failure::DisconnectError,
            Failure::DisconnectPanic,
        ] {
            let storage = Arc::new(SubscriptionStorage::new());
            let drops = Arc::new(AtomicUsize::new(0));
            let other = SubscriptionProbe::insert(&storage, &drops, None, Failure::None);
            let key = SubscriptionProbe::insert(&storage, &drops, Some(other), failure);
            let result = catch_no_unwind(AssertUnwindSafe(|| {
                let mut checked_out = storage.checkout(key).unwrap();
                checked_out.take().disconnect_and_wait()
            }));
            match failure {
                Failure::None => assert!(matches!(result, Ok(Ok(())))),
                Failure::DisconnectError => {
                    assert!(matches!(result, Ok(Err(XllError::Overloaded))));
                }
                Failure::DisconnectPanic => assert!(result.is_err()),
                Failure::CancelPanic => unreachable!(),
            }
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert!(matches!(storage.checkout(key), Err(XllError::StaleHandle)));
            assert!(storage.checkout(other).unwrap().value().cancelled.get());

            let reused = storage.reserve().unwrap();
            assert_eq!(reused.key.index, key.index);
            assert_eq!(reused.key.generation.get(), key.generation.get() + 1);
            drop(reused);
            let mut checked_out = storage.checkout(other).unwrap();
            checked_out.take().disconnect_and_wait().unwrap();
            drop(checked_out);
            assert_eq!(drops.load(Ordering::Relaxed), 2);
            assert!(storage.is_empty());
        }
    }

    #[test]
    fn stale_keys_cannot_checkout_reused_or_out_of_range_slots() {
        let storage = SubscriptionStorage::new();
        let first = storage.reserve().unwrap().publish(11);
        assert_eq!(take(&storage, first), 11);
        let second = storage.reserve().unwrap().publish(12);
        assert_eq!(first.index, second.index);
        assert_ne!(first.generation, second.generation);
        assert!(matches!(
            storage.checkout(first),
            Err(XllError::StaleHandle)
        ));
        assert!(matches!(
            storage.checkout(SlotKey {
                index: usize::MAX,
                ..second
            }),
            Err(XllError::StaleHandle)
        ));
        assert_eq!(take(&storage, second), 12);
        assert!(storage.is_empty());
    }

    #[test]
    fn saturated_generation_becomes_tombstone_instead_of_wrapping_identity() {
        let storage = SubscriptionStorage::new();
        let first = storage.reserve().unwrap().publish(1);
        assert_eq!(take(&storage, first), 1);
        storage.slots.lock().entries[first.index].generation =
            NonZeroU64::new(u64::MAX - 1).unwrap();

        let saturated = storage.reserve().unwrap().publish(2);
        assert_eq!(saturated.index, first.index);
        assert_eq!(saturated.generation.get(), u64::MAX);
        assert_eq!(take(&storage, saturated), 2);
        assert!(storage.is_empty());
        assert!(matches!(
            storage.checkout(first),
            Err(XllError::StaleHandle)
        ));
        assert!(matches!(
            storage.checkout(saturated),
            Err(XllError::StaleHandle)
        ));

        let fresh = storage.reserve().unwrap().publish(3);
        assert_ne!(fresh.index, saturated.index);
        assert_eq!(fresh.generation, NonZeroU64::MIN);
        assert_eq!(take(&storage, fresh), 3);
        let reused = storage.reserve().unwrap().publish(4);
        assert_eq!(reused.index, fresh.index);
        assert_eq!(take(&storage, reused), 4);
        assert!(storage.is_empty());
    }

    #[test]
    fn send_non_sync_values_move_between_workers_and_ticket_stays_affine() {
        static_assertions::assert_impl_all!(SubscriptionStorage<Cell<usize>>: Send, Sync);
        static_assertions::assert_impl_all!(CheckedOut<'static, Cell<usize>>: Send);
        static_assertions::assert_not_impl_any!(CheckedOut<'static, Cell<usize>>: Sync);
        static_assertions::assert_not_impl_any!(SubscriptionStorage<std::rc::Rc<()>>: Send, Sync);
        static_assertions::assert_impl_all!(SourceSubscription: Send);
        static_assertions::assert_not_impl_any!(SourceSubscription: Sync, Clone, Copy);

        let storage = SubscriptionStorage::new();
        let key = storage.reserve().unwrap().publish(Cell::new(5));
        std::thread::scope(|scope| {
            let checked_out = storage.checkout(key).unwrap();
            scope
                .spawn(move || checked_out.value().set(17))
                .join()
                .unwrap();
        });
        assert_eq!(take(&storage, key).get(), 17);
        assert!(storage.is_empty());
    }

    #[derive(Default)]
    struct TicketCounts {
        cancelled: AtomicUsize,
        disconnected: AtomicUsize,
        dropped: AtomicUsize,
    }

    struct TicketValue {
        counts: Arc<TicketCounts>,
        failure: Failure,
    }

    // SAFETY: this fixture owns no sink and starts no worker.
    unsafe impl RtdSubscription for TicketValue {
        fn request_cancel(&self) {
            self.counts.cancelled.fetch_add(1, Ordering::Relaxed);
            if matches!(self.failure, Failure::CancelPanic) {
                panic!("injected ticket cancellation panic");
            }
        }

        fn disconnect_and_wait(self) -> XllResult<()> {
            self.counts.disconnected.fetch_add(1, Ordering::Relaxed);
            match self.failure {
                Failure::DisconnectError => Err(XllError::Overloaded),
                Failure::DisconnectPanic => panic!("injected ticket disconnect panic"),
                Failure::None | Failure::CancelPanic => Ok(()),
            }
        }
    }

    impl Drop for TicketValue {
        fn drop(&mut self) {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct TicketSource;

    // SAFETY: subscribe never retains a sink or starts a worker.
    unsafe impl RtdSource for TicketSource {
        type Value = i32;
        type Subscription = TicketValue;

        fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<TicketValue> {
            Err(XllError::Overloaded)
        }
    }

    fn seeded_ticket(
        adapter: &SourceAdapter<TicketSource>,
        counts: &Arc<TicketCounts>,
        failure: Failure,
    ) -> SourceSubscription {
        let key = adapter
            .subscriptions
            .reserve()
            .unwrap()
            .publish(TicketValue {
                counts: Arc::clone(counts),
                failure,
            });
        // Both tests consume every ticket before its stack-owned adapter.
        SourceSubscription {
            source: std::ptr::NonNull::from(adapter as &dyn ErasedRtdSource),
            key: Some(key),
            _not_sync: std::marker::PhantomData,
        }
    }

    #[test]
    fn source_ticket_drop_contains_disconnect_failure_and_drains_slot_once() {
        for failure in [
            Failure::None,
            Failure::DisconnectError,
            Failure::DisconnectPanic,
        ] {
            let adapter = SourceAdapter {
                subscriptions: SubscriptionStorage::new(),
                source: TicketSource,
            };
            let counts = Arc::new(TicketCounts::default());
            let ticket = seeded_ticket(&adapter, &counts, failure);
            assert!(catch_no_unwind(AssertUnwindSafe(|| drop(ticket))).is_ok());
            assert!(adapter.subscriptions.is_empty());
            assert_eq!(counts.cancelled.load(Ordering::Relaxed), 0);
            assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
            assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn source_ticket_cancel_panic_restores_owner_and_consuming_panic_never_retries() {
        let adapter = SourceAdapter {
            subscriptions: SubscriptionStorage::new(),
            source: TicketSource,
        };
        let counts = Arc::new(TicketCounts::default());
        let mut ticket = seeded_ticket(&adapter, &counts, Failure::CancelPanic);
        assert!(catch_no_unwind(AssertUnwindSafe(|| ticket.request_cancel())).is_err());
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 0);
        drop(ticket);
        assert!(adapter.subscriptions.is_empty());
        assert_eq!(counts.cancelled.load(Ordering::Relaxed), 1);
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);

        let counts = Arc::new(TicketCounts::default());
        let ticket = seeded_ticket(&adapter, &counts, Failure::DisconnectPanic);
        assert!(catch_no_unwind(AssertUnwindSafe(|| ticket.disconnect_and_wait())).is_err());
        assert!(adapter.subscriptions.is_empty());
        assert_eq!(counts.cancelled.load(Ordering::Relaxed), 0);
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }
}
