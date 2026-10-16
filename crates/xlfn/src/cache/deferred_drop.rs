//! Owned destruction batches deferred until initialization and the outer cache
//! operation leave. A batch must not borrow its cache: a different cache may be
//! destroyed by a callback before the destruction queue is flushed.

use std::cell::RefCell;
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
    static PENDING: std::mem::ManuallyDrop<RefCell<Vec<DeferredDrop>>> =
        const { std::mem::ManuallyDrop::new(RefCell::new(Vec::new())) };
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
}

pub(super) fn flush() {
    // Drop outside the TLS borrow so destructors may initialize another cache.
    let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    drop(pending);
}
