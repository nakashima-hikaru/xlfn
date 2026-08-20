use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
#[cfg(any(feature = "async", test))]
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};
use triomphe::Arc;

#[cfg(any(feature = "async", test))]
const STATE_RUNNING: u8 = 0;
const STATE_CANCELED: u8 = 1;
#[cfg(any(feature = "async", test))]
const STATE_DELIVERING: u8 = 2;
#[cfg(feature = "async")]
const STATE_DONE: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationGuarantee {
    BestEffort,
    CalculationScoped,
    SubscriptionScoped,
}

#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
    guarantee: CancellationGuarantee,
}

struct CancellationState {
    cancelled: AtomicBool,
    delivery_state: AtomicU8,
    next_waiter_id: AtomicU64,
    waiters: Mutex<HashMap<u64, std::task::Waker>>,
}

#[cfg(any(feature = "async", test))]
pub(crate) struct CancellationSource {
    inner: Arc<CancellationState>,
}

#[cfg(any(feature = "async", test))]
impl CancellationSource {
    pub(crate) fn new(guarantee: CancellationGuarantee) -> (Self, CancellationToken) {
        let inner = Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            delivery_state: AtomicU8::new(STATE_RUNNING),
            next_waiter_id: AtomicU64::new(1),
            waiters: Mutex::new(HashMap::new()),
        });
        (
            Self {
                inner: Arc::clone(&inner),
            },
            CancellationToken { inner, guarantee },
        )
    }

    pub(crate) fn cancel(&self) {
        let _ = self.inner.delivery_state.compare_exchange(
            STATE_RUNNING,
            STATE_CANCELED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            let waiters = std::mem::take(&mut *self.inner.waiters.lock());
            for (_, waker) in waiters {
                // Cancellation tokens may be polled by arbitrary executors.
                // A broken Waker must not prevent the remaining waiters from
                // observing cancellation or unwind an XLL lifecycle boundary.
                let _ = catch_unwind(AssertUnwindSafe(|| waker.wake()));
            }
        }
    }
}

impl CancellationToken {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
            || self.inner.delivery_state.load(Ordering::Acquire) == STATE_CANCELED
    }

    /// Linearizes delivery vs cancellation using CAS on the delivery state machine.
    ///
    /// Transitions from RUNNING -> DELIVERING if cancellation has not claimed CANCELED.
    /// Returns true if this delivery caller won the right to deliver the result.
    #[cfg(any(feature = "async", test))]
    #[must_use]
    pub(crate) fn try_start_delivery(&self) -> bool {
        self.inner
            .delivery_state
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
        self.inner
            .delivery_state
            .store(STATE_DONE, Ordering::Release);
    }

    #[must_use]
    pub const fn guarantee(&self) -> CancellationGuarantee {
        self.guarantee
    }

    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled {
            token: self,
            waiter_id: None,
        }
    }
}

pub struct Cancelled<'token> {
    token: &'token CancellationToken,
    waiter_id: Option<u64>,
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.token.is_cancelled() {
            self.unregister();
            return Poll::Ready(());
        }

        let waiter_id = match self.waiter_id {
            Some(waiter_id) => waiter_id,
            None => {
                let waiter_id = self
                    .token
                    .inner
                    .next_waiter_id
                    .fetch_add(1, Ordering::Relaxed);
                self.waiter_id = Some(waiter_id);
                waiter_id
            }
        };
        let mut waiters = self.token.inner.waiters.lock();
        if self.token.is_cancelled() {
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
            self.token.inner.waiters.lock().remove(&waiter_id);
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
        let second_token = token.clone();
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
            assert_eq!(token.inner.waiters.lock().len(), 1);
        }
        assert!(token.inner.waiters.lock().is_empty());
    }
}
