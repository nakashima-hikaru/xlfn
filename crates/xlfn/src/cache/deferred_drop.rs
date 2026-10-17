//! Owned destruction batches deferred until initialization and the outer cache
//! operation leave. A batch must not borrow its cache: a different cache may be
//! destroyed by a callback before the destruction queue is flushed.

use smallvec::SmallVec;
use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

struct DeferredDrop {
    allocation: NonNull<()>,
    destroy: unsafe fn(NonNull<()>),
}

impl Drop for DeferredDrop {
    fn drop(&mut self) {
        // SAFETY: defer transfers this unique allocation and its matching drop
        // function; the batch owns no references to an initializer's stack.
        unsafe { (self.destroy)(self.allocation) };
    }
}

thread_local! {
    // Scope guards flush every owned batch. Avoid registering a TLS destructor:
    // a later user TLS destructor can legitimately start another cache operation.
    static PENDING: std::mem::ManuallyDrop<RefCell<SmallVec<[DeferredDrop; 1]>>> =
        std::mem::ManuallyDrop::new(RefCell::new(SmallVec::new()));
    static HAS_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Transfers a detached batch into the current thread's destruction queue.
///
/// # Safety
/// `value` must remain valid independently of the cache and operation until
/// the outer operation and initialization guards exit. `destroy` must contain all user panics.
pub(super) unsafe fn defer<T>(value: T, destroy: fn(T)) {
    unsafe fn run<T>(allocation: NonNull<()>) {
        // SAFETY: defer allocated this exact pair and transferred unique ownership.
        let (value, destroy) = *unsafe { Box::from_non_null(allocation.cast::<(T, fn(T))>()) };
        destroy(value);
    }
    let allocation = Box::into_non_null(Box::new((value, destroy))).cast();
    PENDING.with(|pending| {
        pending.borrow_mut().push(DeferredDrop {
            allocation,
            destroy: run::<T>,
        })
    });
    HAS_PENDING.set(true);
}

pub(super) fn flush() {
    if !HAS_PENDING.replace(false) {
        return;
    }
    // Drop outside the TLS borrow so destructors may initialize another cache.
    // Reset the hint before callbacks: a nested operation owns its own queue
    // and must not have its new hint overwritten by this flush's completion.
    // Taking also frees spilled storage; retaining it in this destructor-free
    // TLS would leak its allocation when the thread exits.
    let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    drop(pending);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn assert_empty_inline_queue() {
        assert!(!HAS_PENDING.get());
        PENDING.with(|pending| {
            let pending = pending.borrow();
            assert!(pending.is_empty());
            assert!(!pending.spilled());
            assert_eq!(pending.capacity(), 1);
        });
    }

    #[test]
    fn empty_flush_does_not_borrow_the_queue() {
        assert_empty_inline_queue();
        PENDING.with(|pending| {
            let _borrow = pending.borrow_mut();
            flush();
            flush();
        });
        assert_empty_inline_queue();
    }

    struct NestedDrop {
        depth: usize,
        drops: Arc<[AtomicUsize; 3]>,
    }

    impl Drop for NestedDrop {
        fn drop(&mut self) {
            self.drops[self.depth].fetch_add(1, Ordering::Relaxed);
            if self.depth != 0 {
                // SAFETY: the batch owns its counters and only recursively
                // enqueues another owned, nonpanicking batch.
                unsafe {
                    defer(
                        Self {
                            depth: self.depth - 1,
                            drops: Arc::clone(&self.drops),
                        },
                        drop::<Self>,
                    );
                }
                flush();
            }
        }
    }

    #[test]
    fn miri_reentrant_defer_and_flush_destroy_every_batch_once() {
        let drops = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
        // SAFETY: NestedDrop owns all its data and its destructor does not panic.
        unsafe {
            defer(
                NestedDrop {
                    depth: 2,
                    drops: Arc::clone(&drops),
                },
                drop::<NestedDrop>,
            );
        }
        assert!(HAS_PENDING.get());
        PENDING.with(|pending| assert!(!pending.borrow().spilled()));
        flush();
        flush();
        assert!(drops.iter().all(|drops| drops.load(Ordering::Relaxed) == 1));
        assert_empty_inline_queue();
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn miri_reentrant_defer_keeps_the_new_pending_hint_until_the_next_flush() {
        struct DefersOnDrop(Arc<AtomicUsize>);
        impl Drop for DefersOnDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
                // SAFETY: this new batch owns its counter and cannot panic.
                unsafe { defer(DropProbe(Arc::clone(&self.0)), drop::<DropProbe>) };
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: DefersOnDrop owns its data and only enqueues a nonpanicking batch.
        unsafe { defer(DefersOnDrop(Arc::clone(&drops)), drop::<DefersOnDrop>) };
        flush();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(HAS_PENDING.get());
        flush();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_empty_inline_queue();
    }

    #[test]
    fn spilled_flush_destroys_once_and_releases_capacity_on_every_cycle() {
        let drops = Arc::new(AtomicUsize::new(0));
        for cycle in 1..=3 {
            for _ in 0..8 {
                // SAFETY: DropProbe independently owns its counter and cannot panic.
                unsafe { defer(DropProbe(Arc::clone(&drops)), drop::<DropProbe>) };
            }
            PENDING.with(|pending| assert!(pending.borrow().spilled()));
            flush();
            assert_eq!(drops.load(Ordering::Relaxed), cycle * 8);
            assert_empty_inline_queue();
        }
    }

    #[test]
    fn contained_destructor_panic_does_not_skip_later_batches() {
        struct PanickingDrop(Arc<AtomicUsize>);
        impl Drop for PanickingDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
                panic!("injected deferred payload destructor panic");
            }
        }
        fn destroy(value: PanickingDrop) {
            assert!(
                crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                    drop(value);
                }))
                .is_err()
            );
        }

        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: the callback contains the payload destructor's injected panic.
        unsafe { defer(PanickingDrop(Arc::clone(&drops)), destroy) };
        // SAFETY: DropProbe owns its counter and does not panic.
        unsafe { defer(DropProbe(Arc::clone(&drops)), drop::<DropProbe>) };
        flush();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_empty_inline_queue();
    }
}
