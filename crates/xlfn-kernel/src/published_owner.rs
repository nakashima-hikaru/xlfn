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
            pointer: Box::into_non_null(value),
        }
    }

    /// Returns the retained non-owning pointer with its provenance and metadata.
    ///
    /// Copying the pointer does not extend the allocation's lifetime.
    /// Dereferencing it requires the usual aliasing and lifetime guarantees;
    /// in particular, callers cannot mutate through it while shared borrows
    /// of the same data remain live.
    ///
    /// [PO-3] Exposes the pointer value without moving or consuming owner capability.
    #[inline]
    pub fn as_non_null(&self) -> NonNull<T> {
        self.pointer
    }

    /// Returns the retained non-owning pointer in raw form.
    /// See [Self::as_non_null] for the access requirements.
    pub fn as_ptr(&self) -> *mut T {
        self.as_non_null().as_ptr()
    }

    /// Recovers ordinary exclusive ownership. Before calling, the publication
    /// protocol must drain every raw capability that could access this value.
    /// Rust borrows obtained through Deref already enforce that restriction.
    ///
    /// [PO-4/PO-6] The protocol model describes this ownership transfer and its
    /// reader-drain precondition. The production Box recovery and callers are
    /// not yet mechanically connected to that model.
    pub fn into_box(self) -> Box<T> {
        let owner = std::mem::ManuallyDrop::new(self);
        // SAFETY: [PO-1] this unique owner consumes the original Box allocation once.
        unsafe { Box::from_non_null(owner.pointer) }
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
    /// Destroys the unique retained allocation. Raw publication lifetimes are
    /// an obligation of the publishing caller, as with Box.
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of the original Box allocation. Raw
        // pointer publishers must end access before owner destruction.
        unsafe { drop(Box::from_non_null(self.pointer)) };
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
        let published = AtomicPtr::new(owner.as_non_null().as_ptr());
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
    fn miri_unsized_trait_pointer_survives_owner_and_parent_box_moves() {
        trait Counter: Send + Sync {
            fn increment(&self);
            fn value(&self) -> usize;
        }
        struct Probe {
            value: AtomicUsize,
            drops: std::sync::Arc<AtomicUsize>,
        }
        impl Counter for Probe {
            fn increment(&self) {
                self.value.fetch_add(1, Ordering::Relaxed);
            }
            fn value(&self) -> usize {
                self.value.load(Ordering::Relaxed)
            }
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        struct Container {
            owner: Option<PublishedOwner<dyn Counter>>,
        }

        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let boxed: Box<dyn Counter> = Box::new(Probe {
            value: AtomicUsize::new(0),
            drops: std::sync::Arc::clone(&drops),
        });
        let owner = PublishedOwner::from_box(boxed);
        let port = owner.as_non_null();
        // Publish the immutable fat-pointer slot through an atomic thin pointer
        // so the reader retains its vtable metadata without unsafe Send wrappers.
        let published = AtomicPtr::new((&raw const port).cast_mut());
        let mut owners = vec![Container { owner: Some(owner) }];
        let original = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                // SAFETY: port is immutable and remains live until this scoped
                // thread joins. Its allocation is retained by the moving owner.
                let port = unsafe { *published.load(Ordering::Acquire) };
                // SAFETY: every container/owner move preserves this allocation;
                // recovery and destruction happen only after the reader joins.
                let counter = unsafe { port.as_ref() };
                for _ in 0..64 {
                    counter.increment();
                }
            });
            for _ in 0..64 {
                owners.push(Container { owner: None });
            }
            let moved = owners.remove(0);
            let mut parent = Box::new(moved);
            // Moving the parent Box must not invalidate pointers to its raw-
            // owned child, unlike retaining a nested ordinary child Box.
            let mut moved_parent = Some(parent);
            parent = moved_parent.take().unwrap();
            reader.join().unwrap();
            parent.owner.take().unwrap()
        });
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        let recovered = original.into_box();
        assert_eq!(recovered.value(), 64);
        drop(recovered);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_non_null_slice_keeps_metadata_through_owner_move() {
        let owner = PublishedOwner::from_box(vec![2_u32, 3, 5].into_boxed_slice());
        let pointer = owner.as_non_null();
        let moved = std::hint::black_box(owner);
        assert_eq!(pointer.len(), 3);
        assert!(std::ptr::eq(pointer.as_ptr(), moved.as_ptr()));
        // SAFETY: the moved owner retains the complete slice.
        assert_eq!(unsafe { pointer.as_ref() }, &[2, 3, 5]);
        assert_eq!(*moved.into_box(), [2, 3, 5]);
    }

    #[test]
    fn miri_non_null_owner_preserves_overaligned_zero_sized_pointer() {
        #[repr(align(256))]
        struct Aligned;

        let owner = PublishedOwner::new(Aligned);
        let pointer = owner.as_non_null();
        let moved = std::hint::black_box(owner);
        assert_eq!(pointer.addr().get() % align_of::<Aligned>(), 0);
        assert_eq!(pointer.as_ptr(), moved.as_ptr());
        // SAFETY: the owner retains the aligned zero-sized payload.
        let _value = unsafe { pointer.as_ref() };
        drop(moved.into_box());
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
