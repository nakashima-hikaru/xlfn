use crate::panic_boundary::{CaughtPanic, catch_no_unwind};
use std::future::Future;
use std::mem::ManuallyDrop;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Keeps user destruction inside a panic boundary, including when a task is
/// cancelled before its first poll or a runnable is reclaimed without polling.
/// The future stays inline and is destroyed in place, preserving its pin.
pub(super) struct NoUnwindFuture<F> {
    future: ManuallyDrop<F>,
    udf_id: &'static str,
    destroyed: bool,
}

impl<F> NoUnwindFuture<F> {
    pub(super) fn new(udf_id: &'static str, future: F) -> Self {
        Self {
            future: ManuallyDrop::new(future),
            udf_id,
            destroyed: false,
        }
    }

    pub(super) fn finish(self: Pin<&mut Self>) -> Result<(), CaughtPanic> {
        // SAFETY: destroy only drops the pinned field in place; it never moves it.
        unsafe { self.get_unchecked_mut() }.destroy()
    }

    fn destroy(&mut self) -> Result<(), CaughtPanic> {
        if self.destroyed {
            return Ok(());
        }
        // Mark before entering user Drop so unwinding can never destroy it twice.
        self.destroyed = true;
        catch_no_unwind(AssertUnwindSafe(|| {
            // SAFETY: this is the unique destruction of the initialized field,
            // at the same address at which any preceding polls pinned it.
            unsafe { ManuallyDrop::drop(&mut self.future) };
        }))
    }
}

impl<F: Future> Future for NoUnwindFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: the field is never moved before destruction, and this wrapper
        // is pinned for every delegated poll.
        let this = unsafe { self.get_unchecked_mut() };
        if this.destroyed {
            xlfn_kernel::invariant::fail_stop();
        }
        // SAFETY: the enclosing pin keeps F at this address until destroy drops
        // it in place, even after this reborrow ends. The pointer being pinned
        // is &mut F, whose pointer traits uphold PinSafePointer's contract;
        // ManuallyDrop is only dereferenced before constructing that pointer.
        unsafe { Pin::new_unchecked(&mut *this.future) }.poll(context)
    }
}

impl<F> Drop for NoUnwindFuture<F> {
    fn drop(&mut self) {
        if self.destroy().is_err() {
            crate::diagnostics::report_no_unwind(self.udf_id, &crate::XllError::Panic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn miri_future_destruction_preserves_pin_and_runs_exactly_once() {
        struct PinnedFuture {
            address: AtomicUsize,
            drops: Arc<AtomicUsize>,
            panic_on_drop: bool,
            _pin: std::marker::PhantomPinned,
        }
        impl Future for PinnedFuture {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                self.address
                    .store(std::ptr::from_ref(&*self).addr(), Ordering::Relaxed);
                Poll::Ready(())
            }
        }
        impl Drop for PinnedFuture {
            fn drop(&mut self) {
                assert_eq!(
                    self.address.load(Ordering::Relaxed),
                    std::ptr::from_ref(self).addr()
                );
                self.drops.fetch_add(1, Ordering::Relaxed);
                if self.panic_on_drop {
                    panic!("injected pinned future destructor panic");
                }
            }
        }

        for panic_on_drop in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            {
                let future = PinnedFuture {
                    address: AtomicUsize::new(0),
                    drops: Arc::clone(&drops),
                    panic_on_drop,
                    _pin: std::marker::PhantomPinned,
                };
                let mut future = std::pin::pin!(NoUnwindFuture::new("pinned future drop", future));
                let waker = futures_util::task::noop_waker();
                assert!(
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(&waker))
                        .is_ready()
                );
                let expected = if panic_on_drop {
                    Err(CaughtPanic)
                } else {
                    Ok(())
                };
                assert_eq!(future.as_mut().finish(), expected);
                assert_eq!(future.as_mut().finish(), Ok(()));
            }
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn future_destruction_retains_panicking_payload_without_running_its_drop() {
        struct PanickingDrop {
            drops: Arc<AtomicUsize>,
            payload_drops: Arc<AtomicUsize>,
        }
        impl Drop for PanickingDrop {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                    &self.payload_drops,
                )));
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let future = PanickingDrop {
            drops: Arc::clone(&drops),
            payload_drops: Arc::clone(&payload_drops),
        };
        let mut future = std::pin::pin!(NoUnwindFuture::new("future drop payload", future));
        assert_eq!(future.as_mut().finish(), Err(CaughtPanic));
        assert_eq!(future.as_mut().finish(), Ok(()));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(payload_drops.load(Ordering::Relaxed), 0);
    }
}
