use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};

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
    next_waiter_id: AtomicU64,
    waiters: Mutex<FxHashMap<u64, std::task::Waker>>,
}

#[allow(
    clippy::vec_box,
    reason = "Boxes guarantee stable heap addresses when the slots vector grows"
)]
struct CancellationRegistryState {
    slots: Vec<Box<CancellationSlot>>,
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
            let generation = slot.generation.fetch_add(1, Ordering::SeqCst) + 1;
            slot.source_live.store(true, Ordering::Release);
            slot.cancelled.store(false, Ordering::Release);
            slot.delivery_state.store(STATE_RUNNING, Ordering::Release);
            slot.next_waiter_id.store(1, Ordering::Release);
            (NonNull::from(&**slot), generation, index)
        } else {
            let index = state.slots.len() as u32;
            let slot = Box::new(CancellationSlot {
                generation: AtomicU64::new(1),
                source_live: AtomicBool::new(true),
                cancelled: AtomicBool::new(false),
                delivery_state: AtomicU8::new(STATE_RUNNING),
                next_waiter_id: AtomicU64::new(1),
                waiters: Mutex::new(FxHashMap::default()),
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
            if slot.generation.load(Ordering::Acquire) == expected_gen {
                slot.source_live.store(false, Ordering::Release);
                let waiters = std::mem::take(&mut *slot.waiters.lock());
                state.free.push(slot_index);
                waiters
            } else {
                FxHashMap::default()
            }
        };
        for (_, waker) in waiters {
            let _ = catch_unwind(AssertUnwindSafe(|| waker.wake()));
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
        if slot.generation.load(Ordering::Acquire) != self.generation {
            return;
        }
        let _ = slot.delivery_state.compare_exchange(
            STATE_RUNNING,
            STATE_CANCELED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if !slot.cancelled.swap(true, Ordering::AcqRel) {
            let waiters = std::mem::take(&mut *slot.waiters.lock());
            for (_, waker) in waiters {
                let _ = catch_unwind(AssertUnwindSafe(|| waker.wake()));
            }
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
        if slot.generation.load(Ordering::Acquire) != self.generation {
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
        if slot.generation.load(Ordering::Acquire) == self.generation {
            slot.delivery_state.store(STATE_DONE, Ordering::Release);
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
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.token.slot.as_ref() };
        if slot.generation.load(Ordering::Acquire) != self.token.generation
            || self.token.is_cancelled()
            || !slot.source_live.load(Ordering::Acquire)
        {
            self.unregister();
            return Poll::Ready(());
        }

        let waiter_id = match self.waiter_id {
            Some(waiter_id) => waiter_id,
            None => {
                let waiter_id = slot.next_waiter_id.fetch_add(1, Ordering::Relaxed);
                self.waiter_id = Some(waiter_id);
                waiter_id
            }
        };
        let mut waiters = slot.waiters.lock();
        if slot.generation.load(Ordering::Acquire) != self.token.generation
            || self.token.is_cancelled()
            || !slot.source_live.load(Ordering::Acquire)
        {
            waiters.remove(&waiter_id);
            drop(waiters);
            self.waiter_id = None;
            Poll::Ready(())
        } else {
            match waiters.get_mut(&waiter_id) {
                Some(waker) if !waker.will_wake(context.waker()) => {
                    *waker = context.waker().clone();
                }
                None => {
                    waiters.insert(waiter_id, context.waker().clone());
                }
                Some(_) => {}
            }
            Poll::Pending
        }
    }
}

impl Cancelled<'_> {
    fn unregister(&mut self) {
        if let Some(waiter_id) = self.waiter_id.take() {
            // SAFETY: slot memory is stable for the lifetime of the process.
            let slot = unsafe { self.token.slot.as_ref() };
            let mut waiters = slot.waiters.lock();
            if slot.generation.load(Ordering::Acquire) == self.token.generation {
                waiters.remove(&waiter_id);
            }
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
            assert_eq!(slot.waiters.lock().len(), 1);
        }
        // SAFETY: slot memory is stable.
        let slot = unsafe { token.slot.as_ref() };
        assert!(slot.waiters.lock().is_empty());
    }

    #[test]
    fn terminal_token_after_source_drop_is_ready_on_poll() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
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
        let (source1, token1) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let gen1 = token1.generation;
        let slot_ptr1 = token1.slot;
        drop(source1);
        assert!(!token1.is_cancelled());

        let (source2, token2) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        assert_eq!(token2.slot, slot_ptr1);
        assert_eq!(token2.generation, gen1 + 1);
        assert!(!token2.is_cancelled());
        assert!(token1.is_cancelled());
        drop(source2);
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
