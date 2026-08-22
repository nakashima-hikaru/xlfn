//! The typed access boundary for registry-owned handle payloads.
//!
//! The registry and epoch code may carry raw publication pointers, but only
//! this module turns those pointers into references. Keeping the two lifetime
//! witnesses here makes the unsafe dereference surface small and auditable:
//! one wrapper is borrowed for an Excel call and the other owns a registry
//! pin for a long-lived handle.

use super::object_store::ObjectPin;
use super::reclamation::EpochReadGuard;
use std::ops::Deref;
use std::ptr::NonNull;

/// A typed object protected by a call-local epoch witness.
pub(crate) struct BorrowedObject<'call, T> {
    ptr: NonNull<T>,
    guard: EpochReadGuard<'call>,
}

impl<'call, T> BorrowedObject<'call, T> {
    pub(crate) fn new(ptr: NonNull<T>, guard: EpochReadGuard<'call>) -> Self {
        Self { ptr, guard }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn address(&self) -> usize {
        self.ptr.as_ptr().addr()
    }
}

impl<T> Deref for BorrowedObject<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        let _epoch = self.guard;
        // SAFETY: construction requires a typed publication pointer and the
        // call-local epoch witness that prevents reclamation for this borrow.
        unsafe { self.ptr.as_ref() }
    }
}

/// A typed object protected by an owned registry lease.
pub(crate) struct ObjectLease<T> {
    ptr: NonNull<T>,
    _pin: ObjectPin,
}

impl<T> ObjectLease<T> {
    pub(crate) fn from_parts(pin: ObjectPin, ptr: NonNull<T>) -> Self {
        Self { ptr, _pin: pin }
    }

    #[inline]
    pub(crate) fn object_id(&self) -> u64 {
        self._pin.object_id().0
    }
}

impl<T> Deref for ObjectLease<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: the registry pin keeps the sole payload owner alive for the
        // entire lifetime of this wrapper.
        unsafe { self.ptr.as_ref() }
    }
}

// SAFETY: the pin and the payload type provide the ownership and thread-safety
// guarantees required to move or share this pointer.
unsafe impl<T: Send + Sync> Send for ObjectLease<T> {}

// SAFETY: same invariant as `Send`.
unsafe impl<T: Send + Sync> Sync for ObjectLease<T> {}
