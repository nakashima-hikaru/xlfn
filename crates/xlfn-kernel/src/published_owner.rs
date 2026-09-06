//! Unique allocation ownership compatible with published, non-owning pointers.
//!
//! Moving a `Box<T>` can retag its allocation as uniquely borrowed. Once raw
//! readers have been published, even moving that Box into a map/state variant
//! can therefore invalidate readers or race their atomic accesses. This owner
//! stores only the pointer after construction and exposes shared access until
//! the owner explicitly recovers a Box after the readers have drained.

#![allow(
    unsafe_code,
    reason = "unique raw allocation owner with explicit publication lifetime"
)]

use std::ops::Deref;
use std::ptr::NonNull;

pub struct PublishedOwner<T: ?Sized> {
    pointer: NonNull<T>,
}

impl<T> PublishedOwner<T> {
    pub fn new(value: T) -> Self {
        Self::from_box(Box::new(value))
    }
}

impl<T: ?Sized> PublishedOwner<T> {
    pub fn from_box(value: Box<T>) -> Self {
        Self {
            // SAFETY: Box allocations are non-null, including zero-sized T.
            pointer: unsafe { NonNull::new_unchecked(Box::into_raw(value)) },
        }
    }

    /// Returns the retained allocation pointer without creating a reference.
    /// Dereferencing it requires the usual aliasing and lifetime guarantees;
    /// in particular, callers cannot mutate through it while shared borrows
    /// of the same data remain live.
    pub fn as_ptr(&self) -> *mut T {
        self.pointer.as_ptr()
    }

    /// Recovers ordinary exclusive ownership. Before calling, the publication
    /// protocol must drain every raw capability that could access this value.
    /// Rust borrows obtained through Deref already enforce that restriction.
    pub fn into_box(self) -> Box<T> {
        let owner = std::mem::ManuallyDrop::new(self);
        // SAFETY: this unique owner consumes the original Box allocation once.
        unsafe { Box::from_raw(owner.pointer.as_ptr()) }
    }
}

impl<T: ?Sized> AsRef<T> for PublishedOwner<T> {
    fn as_ref(&self) -> &T {
        // SAFETY: this owner retains the allocation until its own destruction.
        // There is no mutable-reference API that could invalidate raw readers.
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: ?Sized> Deref for PublishedOwner<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.as_ref()
    }
}

impl<T: ?Sized> Drop for PublishedOwner<T> {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner. As with Box, any raw capability's
        // creator must guarantee it does not survive allocation destruction.
        drop(unsafe { Box::from_raw(self.pointer.as_ptr()) });
    }
}

// SAFETY: transferring the sole owner can transfer destruction of T.
unsafe impl<T: ?Sized + Send> Send for PublishedOwner<T> {}
// SAFETY: shared access exposes only &T.
unsafe impl<T: ?Sized + Sync> Sync for PublishedOwner<T> {}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for PublishedOwner<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PublishedOwner")
            .field(&self.as_ref())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    #[test]
    fn miri_published_pointer_survives_owner_moves_and_container_growth() {
        let owner = PublishedOwner::new(AtomicUsize::new(0));
        let published = AtomicPtr::new(NonNull::from(owner.as_ref()).as_ptr());
        let mut owners = vec![owner];
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                // SAFETY: owners retains this allocation until the thread joins.
                let value = unsafe { &*published.load(Ordering::Acquire) };
                for _ in 0..64 {
                    value.fetch_add(1, Ordering::Relaxed);
                }
            });
            for _ in 0..64 {
                owners.push(PublishedOwner::new(AtomicUsize::new(0)));
            }
            let original = owners.remove(0);
            owners.push(original);
            reader.join().unwrap();
        });
        let original = owners.pop().unwrap().into_box();
        assert_eq!(original.load(Ordering::Relaxed), 64);
    }

    #[test]
    fn miri_owner_drops_payload_once_with_or_without_box_recovery() {
        struct CountDrop<'a>(&'a AtomicUsize);
        impl Drop for CountDrop<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = AtomicUsize::new(0);
        drop(PublishedOwner::new(CountDrop(&drops)));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(PublishedOwner::new(CountDrop(&drops)).into_box());
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
}
