use crate::panic_boundary::catch_no_unwind;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};
use xlfn_kernel::published_owner::PublishedOwner;

const STATE_RUNNING: u8 = 0;
const STATE_CANCELED: u8 = 1;
const STATE_DELIVERING: u8 = 2;
#[cfg(feature = "async")]
const STATE_DONE: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationGuarantee {
    #[cfg(feature = "async")]
    BestEffort,
    CalculationScoped,
    #[cfg(feature = "async")]
    SubscriptionScoped,
}

struct CancellationSlot {
    generation: AtomicU64,
    source_live: AtomicBool,
    cancelled: AtomicBool,
    delivery_state: AtomicU8,
    // Slot reuse and every generation-dependent mutation share this lock. A
    // token's optimistic atomic check never authorizes a mutation by itself.
    waiters: Mutex<SlotWaiters>,
}

struct SlotWaiters {
    generation: u64,
    next_id: u64,
    entries: FxHashMap<u64, std::task::Waker>,
}

struct CancellationRegistryState {
    slots: Vec<PublishedOwner<CancellationSlot>>,
    free: Vec<u32>,
}

pub(crate) struct CancellationRegistry {
    state: Mutex<CancellationRegistryState>,
}

impl CancellationRegistry {
    pub(crate) const fn new() -> Self {
        Self {
            state: parking_lot::const_mutex(CancellationRegistryState {
                slots: Vec::new(),
                free: Vec::new(),
            }),
        }
    }

    fn allocate(&self) -> (NonNull<CancellationSlot>, u64, u32) {
        let mut state = self.state.lock();
        if let Some(index) = state.free.pop() {
            let slot = &state.slots[index as usize];
            let mut waiters = slot.waiters.lock();
            let generation = waiters
                .generation
                .checked_add(1)
                .expect("free slot generation");
            debug_assert!(waiters.entries.is_empty());
            waiters.generation = generation;
            waiters.next_id = 1;
            slot.generation.store(generation, Ordering::Release);
            slot.source_live.store(true, Ordering::Release);
            slot.cancelled.store(false, Ordering::Release);
            slot.delivery_state.store(STATE_RUNNING, Ordering::Release);
            (NonNull::from(&**slot), generation, index)
        } else {
            let index =
                u32::try_from(state.slots.len()).expect("cancellation slot index exhausted");
            let slot = PublishedOwner::new(CancellationSlot {
                generation: AtomicU64::new(1),
                source_live: AtomicBool::new(true),
                cancelled: AtomicBool::new(false),
                delivery_state: AtomicU8::new(STATE_RUNNING),
                waiters: Mutex::new(SlotWaiters {
                    generation: 1,
                    next_id: 1,
                    entries: FxHashMap::default(),
                }),
            });
            let ptr = NonNull::from(&*slot);
            state.slots.push(slot);
            (ptr, 1, index)
        }
    }

    fn release(&self, slot_index: u32, expected_gen: u64) {
        let waiters = {
            let mut state = self.state.lock();
            let slot = &state.slots[slot_index as usize];
            let mut waiters = slot.waiters.lock();
            if waiters.generation != expected_gen {
                return;
            }
            slot.source_live.store(false, Ordering::Release);
            let detached = std::mem::take(&mut waiters.entries);
            drop(waiters);
            // A wrapped generation could make a process-live old token valid
            // again. Exhausted slots remain allocated, but are never reused.
            if expected_gen != u64::MAX {
                state.free.push(slot_index);
            }
            detached
        };
        for (_, waker) in waiters {
            let _ = catch_no_unwind(AssertUnwindSafe(|| waker.wake()));
        }
    }
}

static CANCELLATION_REGISTRY: CancellationRegistry = CancellationRegistry::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationToken {
    slot: NonNull<CancellationSlot>,
    generation: u64,
    guarantee: CancellationGuarantee,
}

// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Send for CancellationToken {}
// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Sync for CancellationToken {}

pub(crate) struct CancellationSource {
    slot: NonNull<CancellationSlot>,
    generation: u64,
    slot_index: u32,
}

// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Send for CancellationSource {}
// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Sync for CancellationSource {}

impl CancellationSource {
    pub(crate) fn new(guarantee: CancellationGuarantee) -> (Self, CancellationToken) {
        let (slot, generation, slot_index) = CANCELLATION_REGISTRY.allocate();
        (
            Self {
                slot,
                generation,
                slot_index,
            },
            CancellationToken {
                slot,
                generation,
                guarantee,
            },
        )
    }

    pub(crate) fn cancel(&self) {
        // SAFETY: self.slot is valid for the lifetime of this CancellationSource.
        let slot = unsafe { self.slot.as_ref() };
        let detached = {
            let mut waiters = slot.waiters.lock();
            if waiters.generation != self.generation {
                return;
            }
            let _ = slot.delivery_state.compare_exchange(
                STATE_RUNNING,
                STATE_CANCELED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if slot.cancelled.swap(true, Ordering::AcqRel) {
                return;
            }
            std::mem::take(&mut waiters.entries)
        };
        for (_, waker) in detached {
            let _ = catch_no_unwind(AssertUnwindSafe(|| waker.wake()));
        }
    }
}

impl Drop for CancellationSource {
    fn drop(&mut self) {
        CANCELLATION_REGISTRY.release(self.slot_index, self.generation);
    }
}

impl CancellationToken {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        if slot.generation.load(Ordering::Acquire) != self.generation {
            return true;
        }
        slot.cancelled.load(Ordering::Acquire)
            || slot.delivery_state.load(Ordering::Acquire) == STATE_CANCELED
            // If the reads observed reset fields, the generation published
            // before those release stores must also be observed here.
            || slot.generation.load(Ordering::Acquire) != self.generation
    }

    /// Linearizes delivery vs cancellation using CAS on the delivery state machine.
    ///
    /// Transitions from RUNNING -> DELIVERING if cancellation has not claimed CANCELED.
    /// Returns true if this delivery caller won the right to deliver the result.
    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn try_start_delivery(&self) -> bool {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        let waiters = slot.waiters.lock();
        if waiters.generation != self.generation || !slot.source_live.load(Ordering::Acquire) {
            return false;
        }
        slot.delivery_state
            .compare_exchange(
                STATE_RUNNING,
                STATE_DELIVERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[cfg(feature = "async")]
    pub(crate) fn finish_delivery(&self) {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        let waiters = slot.waiters.lock();
        if waiters.generation == self.generation {
            let _ = slot.delivery_state.compare_exchange(
                STATE_DELIVERING,
                STATE_DONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    #[must_use]
    pub const fn guarantee(&self) -> CancellationGuarantee {
        self.guarantee
    }

    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled {
            token: *self,
            waiter_id: None,
            _marker: PhantomData,
        }
    }
}

pub struct Cancelled<'token> {
    token: CancellationToken,
    waiter_id: Option<u64>,
    _marker: PhantomData<&'token ()>,
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_before_lock(context, || {})
    }
}

impl Cancelled<'_> {
    // The hook fixes the optimistic-check / slot-reuse interleaving in tests;
    // the production call's no-op is eliminated during monomorphization.
    fn poll_before_lock(
        &mut self,
        context: &mut Context<'_>,
        before_lock: impl FnOnce(),
    ) -> Poll<()> {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.token.slot.as_ref() };
        if self.token.is_cancelled() || !slot.source_live.load(Ordering::Acquire) {
            self.unregister();
            return Poll::Ready(());
        }

        // RawWaker clone/drop callbacks may reenter this slot. Keep the new
        // waker outside the guard's lifetime, including early returns/unwind.
        let mut replacement = Some(context.waker().clone());
        before_lock();
        let mut waiters = slot.waiters.lock();
        if waiters.generation != self.token.generation {
            self.waiter_id = None;
            return Poll::Ready(());
        }
        let detached;
        let result;
        if self.token.is_cancelled() || !slot.source_live.load(Ordering::Acquire) {
            detached = self
                .waiter_id
                .take()
                .and_then(|id| waiters.entries.remove(&id));
            result = Poll::Ready(());
        } else {
            let waiter_id = *self.waiter_id.get_or_insert_with(|| {
                let id = waiters.next_id;
                waiters.next_id = id.checked_add(1).expect("cancellation waiter id exhausted");
                id
            });
            if waiters
                .entries
                .get(&waiter_id)
                .is_some_and(|waker| waker.will_wake(context.waker()))
            {
                detached = None;
            } else {
                detached = waiters
                    .entries
                    .insert(waiter_id, replacement.take().expect("new waker"));
            }
            result = Poll::Pending;
        }
        drop(waiters);
        drop(detached);
        result
    }

    fn unregister(&mut self) {
        if let Some(waiter_id) = self.waiter_id.take() {
            // SAFETY: slot memory is stable for the lifetime of the process.
            let slot = unsafe { self.token.slot.as_ref() };
            let detached = {
                let mut waiters = slot.waiters.lock();
                if waiters.generation == self.token.generation {
                    waiters.entries.remove(&waiter_id)
                } else {
                    None
                }
            };
            drop(detached);
        }
    }
}

impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::task::{ArcWake, noop_waker, waker};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::AtomicUsize;

    // Isolate slot-reuse tests from allocations in concurrently running tests.
    // Every token/future is dropped before this fixture's registry.
    struct LocalSource<'registry> {
        source: std::mem::ManuallyDrop<CancellationSource>,
        registry: &'registry CancellationRegistry,
    }

    impl std::ops::Deref for LocalSource<'_> {
        type Target = CancellationSource;

        fn deref(&self) -> &Self::Target {
            &self.source
        }
    }

    impl Drop for LocalSource<'_> {
        fn drop(&mut self) {
            self.registry
                .release(self.source.slot_index, self.source.generation);
        }
    }

    fn local_source(registry: &CancellationRegistry) -> (LocalSource<'_>, CancellationToken) {
        let (slot, generation, slot_index) = registry.allocate();
        (
            LocalSource {
                source: std::mem::ManuallyDrop::new(CancellationSource {
                    slot,
                    generation,
                    slot_index,
                }),
                registry,
            },
            CancellationToken {
                slot,
                generation,
                guarantee: CancellationGuarantee::CalculationScoped,
            },
        )
    }

    fn owned_waiter(token: CancellationToken) -> Cancelled<'static> {
        Cancelled {
            token,
            waiter_id: None,
            _marker: PhantomData,
        }
    }

    struct ReentrantDrop {
        nested: Option<Cancelled<'static>>,
        token: CancellationToken,
        dropped: StdArc<AtomicUsize>,
    }

    impl ArcWake for ReentrantDrop {
        fn wake_by_ref(_: &StdArc<Self>) {}
    }

    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            // SAFETY: the fixture's registry outlives all of its wakers.
            let slot = unsafe { self.token.slot.as_ref() };
            let unlocked = slot.waiters.try_lock().is_some();
            if !unlocked {
                // Fail promptly instead of hanging inside the nested Drop if
                // this regression returns. The registry still owns its waker.
                if let Some(nested) = &mut self.nested {
                    nested.waiter_id = None;
                }
            }
            assert!(unlocked, "Waker::drop ran while the slot mutex was held");
            drop(self.nested.take());
            self.dropped.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn register_reentrant_drop(
        token: CancellationToken,
        dropped: &StdArc<AtomicUsize>,
    ) -> Cancelled<'static> {
        let mut nested = owned_waiter(token);
        let noop = noop_waker();
        assert!(
            Pin::new(&mut nested)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        let waker = waker(StdArc::new(ReentrantDrop {
            nested: Some(nested),
            token,
            dropped: StdArc::clone(dropped),
        }));
        let mut future = owned_waiter(token);
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        future
    }

    struct WakeCount(AtomicUsize);

    impl ArcWake for WakeCount {
        fn wake_by_ref(arc_self: &StdArc<Self>) {
            arc_self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct PanicFirstWake(AtomicUsize);

    impl ArcWake for PanicFirstWake {
        fn wake_by_ref(arc_self: &StdArc<Self>) {
            if arc_self.0.fetch_add(1, Ordering::AcqRel) == 0 {
                panic!("injected cancellation waker panic");
            }
        }
    }

    #[test]
    fn cancellation_is_sticky_and_observable() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        assert!(!token.is_cancelled());
        source.cancel();
        assert!(token.is_cancelled());
        assert_eq!(token.guarantee(), CancellationGuarantee::CalculationScoped);

        let mut future = std::pin::pin!(token.cancelled());
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
    }

    #[test]
    fn cancellation_wakes_every_registered_waiter() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let first_count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let second_count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let first_waker = waker(StdArc::clone(&first_count));
        let second_waker = waker(StdArc::clone(&second_count));
        let mut first = std::pin::pin!(token.cancelled());
        let second_token = token;
        let mut second = std::pin::pin!(second_token.cancelled());

        assert_eq!(
            first.as_mut().poll(&mut Context::from_waker(&first_waker)),
            Poll::Pending
        );
        assert_eq!(
            second
                .as_mut()
                .poll(&mut Context::from_waker(&second_waker)),
            Poll::Pending
        );

        source.cancel();

        assert_eq!(first_count.0.load(Ordering::Acquire), 1);
        assert_eq!(second_count.0.load(Ordering::Acquire), 1);
        assert_eq!(
            first.as_mut().poll(&mut Context::from_waker(&first_waker)),
            Poll::Ready(())
        );
        assert_eq!(
            second
                .as_mut()
                .poll(&mut Context::from_waker(&second_waker)),
            Poll::Ready(())
        );
    }

    #[test]
    fn panicking_waker_does_not_stop_later_cancellation_notifications() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let wake_state = StdArc::new(PanicFirstWake(AtomicUsize::new(0)));
        let panic_first_waker = waker(StdArc::clone(&wake_state));
        let mut waiters = [
            Box::pin(token.cancelled()),
            Box::pin(token.cancelled()),
            Box::pin(token.cancelled()),
        ];
        for waiter in &mut waiters {
            assert_eq!(
                waiter
                    .as_mut()
                    .poll(&mut Context::from_waker(&panic_first_waker)),
                Poll::Pending
            );
        }

        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| source.cancel())).is_ok(),
            "CancellationSource::cancel must not propagate a user Waker panic"
        );
        assert_eq!(wake_state.0.load(Ordering::Acquire), waiters.len());
        for waiter in &mut waiters {
            assert_eq!(
                waiter
                    .as_mut()
                    .poll(&mut Context::from_waker(&panic_first_waker)),
                Poll::Ready(())
            );
        }
    }

    #[test]
    fn panicking_payload_drop_cannot_interrupt_cancel_or_release_notifications() {
        struct PayloadWake {
            wakes: AtomicUsize,
            payload_drops: StdArc<AtomicUsize>,
        }

        impl ArcWake for PayloadWake {
            fn wake_by_ref(state: &StdArc<Self>) {
                if state.wakes.fetch_add(1, Ordering::AcqRel) == 0 {
                    std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(
                        StdArc::clone(&state.payload_drops),
                    ));
                }
            }
        }

        for release in [false, true] {
            let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
            let payload_drops = StdArc::new(AtomicUsize::new(0));
            let wake_state = StdArc::new(PayloadWake {
                wakes: AtomicUsize::new(0),
                payload_drops: StdArc::clone(&payload_drops),
            });
            let waker = waker(StdArc::clone(&wake_state));
            let mut waiters = [token.cancelled(), token.cancelled(), token.cancelled()];
            for waiter in &mut waiters {
                assert!(
                    Pin::new(waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_pending()
                );
            }
            if release {
                drop(source);
            } else {
                source.cancel();
            }
            assert_eq!(wake_state.wakes.load(Ordering::Acquire), waiters.len());
            assert_eq!(payload_drops.load(Ordering::Acquire), 0);
            for waiter in &mut waiters {
                assert!(
                    Pin::new(waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_ready()
                );
            }
        }
    }

    #[test]
    fn delivery_cas_linearizes_against_cancellation() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        assert!(token.try_start_delivery());
        // Second delivery attempt fails
        assert!(!token.try_start_delivery());

        // Cancel after delivery started cannot transition state to CANCELED
        source.cancel();
    }

    #[test]
    fn cancellation_prevents_delivery_cas() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        source.cancel();
        // Delivery CAS fails because token is CANCELED
        assert!(!token.try_start_delivery());
    }

    #[test]
    fn dropping_waiter_unregisters_it() {
        let (_source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        {
            let mut future = std::pin::pin!(token.cancelled());
            let waker = noop_waker();
            assert_eq!(
                future.as_mut().poll(&mut Context::from_waker(&waker)),
                Poll::Pending
            );
            // SAFETY: slot memory is stable.
            let slot = unsafe { token.slot.as_ref() };
            assert_eq!(slot.waiters.lock().entries.len(), 1);
        }
        // SAFETY: slot memory is stable.
        let slot = unsafe { token.slot.as_ref() };
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn terminal_token_after_source_drop_is_ready_on_poll() {
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        assert!(!token.is_cancelled());
        drop(source);
        assert!(!token.is_cancelled());

        let mut future = std::pin::pin!(token.cancelled());
        let waker = noop_waker();
        assert_eq!(
            future.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Ready(())
        );
    }

    #[test]
    fn slot_reuse_advances_generation_and_leaves_old_token_stale() {
        let registry = CancellationRegistry::new();
        let (source1, token1) = local_source(&registry);
        let gen1 = token1.generation;
        let slot_ptr1 = token1.slot;
        drop(source1);
        assert!(!token1.is_cancelled());

        let (source2, token2) = local_source(&registry);
        assert_eq!(token2.slot, slot_ptr1);
        assert_eq!(token2.generation, gen1 + 1);
        assert!(!token2.is_cancelled());
        assert!(token1.is_cancelled());
        drop(source2);
    }

    #[test]
    fn miri_repoll_across_slot_reuse_preserves_new_generation_waiter() {
        // Cover both a previously registered waiter (the ID collision) and a
        // first poll that reaches registration only after the slot is reused.
        for already_registered in [false, true] {
            let registry = CancellationRegistry::new();
            let (source, old_token) = local_source(&registry);
            let mut old_waiter = old_token.cancelled();
            let noop = noop_waker();
            if already_registered {
                assert!(
                    Pin::new(&mut old_waiter)
                        .poll(&mut Context::from_waker(&noop))
                        .is_pending()
                );
                assert_eq!(old_waiter.waiter_id, Some(1));
            }
            let paused = std::sync::Barrier::new(2);
            let resume = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let old_poll = scope.spawn(|| {
                    old_waiter.poll_before_lock(&mut Context::from_waker(&noop), || {
                        paused.wait();
                        resume.wait();
                    })
                });
                paused.wait();
                drop(source);
                let (next_source, next_token) = local_source(&registry);
                assert_eq!(next_token.slot, old_token.slot);
                let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
                let waker = waker(StdArc::clone(&count));
                let mut next_waiter = next_token.cancelled();
                assert!(
                    Pin::new(&mut next_waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_pending()
                );
                assert_eq!(next_waiter.waiter_id, Some(1));
                resume.wait();
                assert_eq!(old_poll.join().unwrap(), Poll::Ready(()));
                // SAFETY: next_source keeps its fixture slot live.
                let slot = unsafe { next_token.slot.as_ref() };
                assert_eq!(slot.waiters.lock().entries.len(), 1);
                next_source.cancel();
                assert_eq!(count.0.load(Ordering::Acquire), 1);
                assert!(
                    Pin::new(&mut next_waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_ready()
                );
            });
        }
    }

    #[test]
    fn dropping_stale_waiter_preserves_new_generation_waiter() {
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let mut stale = token.cancelled();
        let noop = noop_waker();
        assert!(
            Pin::new(&mut stale)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        drop(source);
        let (next_source, next_token) = local_source(&registry);
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let mut next = next_token.cancelled();
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        drop(stale);
        next_source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn replacing_waker_can_reenter_waiter_unregistration() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        let mut future = register_reentrant_drop(token, &dropped);
        let noop = noop_waker();
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        // SAFETY: the registry outlives the future.
        let slot = unsafe { token.slot.as_ref() };
        assert_eq!(slot.waiters.lock().entries.len(), 1);
    }

    #[test]
    fn dropping_waker_can_reenter_waiter_unregistration() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        drop(register_reentrant_drop(token, &dropped));
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn terminal_repoll_drops_waker_outside_waiter_lock() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        let mut future = register_reentrant_drop(token, &dropped);
        let noop = noop_waker();
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert!(
            future
                .poll_before_lock(&mut Context::from_waker(&noop), || {
                    slot.cancelled.store(true, Ordering::Release);
                })
                .is_ready()
        );
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn raw_waker_clone_and_unused_clone_drop_run_outside_slot_lock() {
        use std::task::{RawWaker, RawWakerVTable, Waker};

        struct RawState {
            token: CancellationToken,
            nested: Mutex<Option<Cancelled<'static>>>,
            clones: AtomicUsize,
            drops: AtomicUsize,
        }

        impl RawState {
            fn reenter(&self) {
                // SAFETY: the fixture registry outlives every raw waker.
                let slot = unsafe { self.token.slot.as_ref() };
                let unlocked = slot.waiters.try_lock().is_some();
                let mut nested = self.nested.lock().take();
                if !unlocked && let Some(nested) = &mut nested {
                    nested.waiter_id = None;
                }
                assert!(
                    unlocked,
                    "RawWaker callback ran while the slot mutex was held"
                );
                drop(nested);
            }
        }

        unsafe fn clone(data: *const ()) -> RawWaker {
            // SAFETY: data is an Arc<RawState> pointer retained by the waker.
            let state = unsafe { &*data.cast::<RawState>() };
            state.reenter();
            state.clones.fetch_add(1, Ordering::AcqRel);
            // SAFETY: the source waker still owns a strong reference.
            unsafe { StdArc::increment_strong_count(data.cast::<RawState>()) };
            RawWaker::new(data, &VTABLE)
        }

        unsafe fn release(data: *const ()) {
            // SAFETY: this callback consumes exactly one raw strong reference.
            let state = unsafe { StdArc::from_raw(data.cast::<RawState>()) };
            state.reenter();
            state.drops.fetch_add(1, Ordering::AcqRel);
        }

        unsafe fn wake_by_ref(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, release, wake_by_ref, release);

        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let noop = noop_waker();
        let mut nested = owned_waiter(token);
        assert!(
            Pin::new(&mut nested)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        let state = StdArc::new(RawState {
            token,
            nested: Mutex::new(Some(nested)),
            clones: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
        });
        let raw = RawWaker::new(StdArc::into_raw(StdArc::clone(&state)).cast(), &VTABLE);
        // SAFETY: the vtable balances Arc references and is thread-safe.
        let waker = unsafe { Waker::from_raw(raw) };
        let mut future = token.cancelled();
        for _ in 0..2 {
            assert!(
                Pin::new(&mut future)
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
        }
        assert_eq!(state.clones.load(Ordering::Acquire), 2);
        assert_eq!(state.drops.load(Ordering::Acquire), 1);
        assert!(state.nested.lock().is_none());

        let mut next_source = None;
        assert!(
            future
                .poll_before_lock(&mut Context::from_waker(&waker), || {
                    drop(source);
                    next_source = Some(local_source(&registry).0);
                })
                .is_ready()
        );
        assert_eq!(state.clones.load(Ordering::Acquire), 3);
        assert_eq!(state.drops.load(Ordering::Acquire), 3);
        drop(next_source);
    }

    #[test]
    fn released_and_stale_tokens_cannot_mutate_delivery_state() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        drop(source);
        assert!(!old_token.try_start_delivery());
        let (_source, token) = local_source(&registry);
        assert!(!old_token.try_start_delivery());
        old_token.finish_delivery();
        assert!(token.try_start_delivery());
        old_token.finish_delivery();
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert_eq!(
            slot.delivery_state.load(Ordering::Acquire),
            STATE_DELIVERING
        );
        token.finish_delivery();
        assert_eq!(slot.delivery_state.load(Ordering::Acquire), STATE_DONE);
    }

    #[test]
    fn exhausted_slot_generation_is_never_reused() {
        let registry = CancellationRegistry::new();
        let (slot_ptr, _, index) = registry.allocate();
        // SAFETY: the registry owns this slot for the duration of the test.
        let slot = unsafe { slot_ptr.as_ref() };
        slot.generation.store(u64::MAX, Ordering::Release);
        slot.waiters.lock().generation = u64::MAX;
        registry.release(index, u64::MAX);
        let (next, generation, _) = registry.allocate();
        assert_ne!(next, slot_ptr);
        assert_eq!(generation, 1);
    }

    #[test]
    fn wake_from_release_can_reenter_cancellation_registry_without_deadlock() {
        struct ReentrantWake;
        impl ArcWake for ReentrantWake {
            fn wake_by_ref(_arc_self: &StdArc<Self>) {
                let (source, token) =
                    CancellationSource::new(CancellationGuarantee::CalculationScoped);
                assert!(!token.is_cancelled());
                drop(source);
            }
        }

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let mut future = std::pin::pin!(token.cancelled());
        let reentrant_waker = waker(StdArc::new(ReentrantWake));
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(&reentrant_waker)),
            Poll::Pending
        );

        drop(source);
    }
}
