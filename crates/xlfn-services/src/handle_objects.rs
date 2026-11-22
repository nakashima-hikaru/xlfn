//! Stable-address payload ownership for formula handles. No host, registry,
//! session, or callback is required to allocate or inspect an object here.
//! Binding counts and quiescence decide when the owner may be dropped.

use std::any::{Any, TypeId, type_name};
use std::marker::PhantomData;
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;

/// A type-checked, non-owning projection into an [`ObjectCell`].
pub struct TypedObjectProjection<T: 'static> {
    pointer: NonNull<T>,
    // The cached TypeId denotes the exact T, even for covariant payloads.
    _type: PhantomData<fn(T) -> T>,
}

impl<T: 'static> Copy for TypedObjectProjection<T> {}

impl<T: 'static> Clone for TypedObjectProjection<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> TypedObjectProjection<T> {
    #[doc(hidden)]
    pub fn addr(&self) -> usize {
        self.pointer.addr().get()
    }

    /// Returns a shared reference to the projected object.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the object's backing cell has not been reclaimed
    /// and remains pinned, leased, or protected by an active read capability for the
    /// entire duration of the returned borrow.
    #[inline]
    pub unsafe fn as_ref_unchecked(&self) -> &T {
        // SAFETY: guaranteed by the caller per contract.
        unsafe { self.pointer.as_ref() }
    }
}

/// Keeps concrete type names available through the vtable for diagnostics,
/// without storing a name alongside every object. Lookup uses the cached TypeId.
trait ErasedObject<A: 'static, I: Copy + 'static>: Any + Send + Sync {
    fn concrete_type_name(&self) -> &'static str;
    fn allocation_destructor(&self) -> unsafe fn(NonNull<ObjectCell<A, I>>);
}

impl<A: 'static, I: Copy + 'static, T: Any + Send + Sync> ErasedObject<A, I> for T {
    fn concrete_type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn allocation_destructor(&self) -> unsafe fn(NonNull<ObjectCell<A, I>>) {
        if combine_object_allocation::<A, I, T>() {
            destroy_combined::<A, I, T>
        } else {
            destroy_separate::<A, I, T>
        }
    }
}

/// Immutable header retained by the unique allocation owner.
///
/// The issuer address is opaque to this layer: neither destruction nor any
/// safe method dereferences it. A higher-level protocol must retain the issuer
/// before turning the address back into a reference.
pub struct ObjectCell<A: 'static, I: Copy + 'static> {
    arena: NonNull<A>,
    id: I,
    payload: NonNull<dyn ErasedObject<A, I>>,
    type_id: TypeId,
}

/// The fixed header stays at offset zero; bindings never carry tail metadata.
#[repr(C)]
struct ObjectAllocation<A: 'static, I: Copy + 'static, T> {
    cell: ObjectCell<A, I>,
    payload: T,
}

const fn combine_object_allocation<A: 'static, I: Copy + 'static, T>() -> bool {
    // Coallocate only when it removes an allocation without introducing any
    // header/tail alignment padding. Zero-sized Box payloads allocate nothing.
    size_of::<T>() != 0
        && match size_of::<ObjectCell<A, I>>().checked_add(size_of::<T>()) {
            Some(size) => size == size_of::<ObjectAllocation<A, I, T>>(),
            None => false,
        }
}

/// Unique erased allocation owner. Unlike a Box, moving this owner cannot
/// retag the published header or payload. Destruction dispatch is shared in the
/// existing payload vtable rather than retained per object or per binding.
pub struct ObjectAllocationOwner<A: 'static, I: Copy + 'static> {
    cell: NonNull<ObjectCell<A, I>>,
}

impl<A: 'static, I: Copy + 'static> ObjectAllocationOwner<A, I> {
    /// Allocates a header and payload at fixed addresses.
    pub fn new<T: Send + Sync + 'static>(arena: NonNull<A>, id: I, value: T) -> Self {
        if !combine_object_allocation::<A, I, T>() {
            return Self::separate(arena, id, value);
        }
        let allocation = PublishedOwner::new(ObjectAllocation {
            cell: ObjectCell {
                arena,
                id,
                payload: NonNull::<T>::dangling(),
                type_id: TypeId::of::<T>(),
            },
            payload: value,
        });
        let pointer = allocation.as_non_null();
        // SAFETY: repr(C) places this nonzero-sized payload at its aligned
        // field offset within the uniquely owned allocation.
        let payload = unsafe {
            pointer
                .byte_add(std::mem::offset_of!(ObjectAllocation<A, I, T>, payload))
                .cast::<T>()
        };
        // repr(C) places cell at offset zero, preserving allocation provenance.
        let cell = pointer.cast::<ObjectCell<A, I>>();
        // SAFETY: this unpublished header is uniquely owned by allocation.
        let payload_slot = unsafe { &raw mut (*cell.as_ptr()).payload };
        // SAFETY: the raw slot points to the live, initialized header field;
        // replacing the initial non-owning pointer cannot drop any payload.
        unsafe { payload_slot.write(payload) };
        // Transfer the unique allocation obligation to the matching concrete
        // destructor without ever recovering or moving a published Box.
        let _allocation = std::mem::ManuallyDrop::new(allocation);
        Self { cell }
    }

    fn separate<T: Send + Sync + 'static>(arena: NonNull<A>, id: I, value: T) -> Self {
        let payload = PublishedOwner::new(value);
        let cell = PublishedOwner::new(ObjectCell {
            arena,
            id,
            payload: payload.as_non_null(),
            type_id: TypeId::of::<T>(),
        });
        let pointer = cell.as_non_null();
        let _payload = std::mem::ManuallyDrop::new(payload);
        let _cell = std::mem::ManuallyDrop::new(cell);
        Self { cell: pointer }
    }

    #[inline]
    pub fn as_non_null(&self) -> NonNull<ObjectCell<A, I>> {
        self.cell
    }

    /// Whether this exact header layout can share one allocation with T.
    #[doc(hidden)]
    pub const fn can_coallocate<T>() -> bool {
        combine_object_allocation::<A, I, T>()
    }

    pub fn cell(&self) -> &ObjectCell<A, I> {
        // SAFETY: the unique owner keeps this header and its payload alive.
        unsafe { self.cell.as_ref() }
    }
}

unsafe fn destroy_combined<A: 'static, I: Copy + 'static, T>(cell: NonNull<ObjectCell<A, I>>) {
    // SAFETY: ObjectAllocation<A, I, T> is repr(C) with cell at offset zero. The
    // owner was constructed for this exact T, and all readers have drained.
    unsafe { drop(Box::from_non_null(cell.cast::<ObjectAllocation<A, I, T>>())) };
}

unsafe fn destroy_separate<A: 'static, I: Copy + 'static, T>(cell: NonNull<ObjectCell<A, I>>) {
    // SAFETY: the separate constructor transferred these exact Box allocations
    // and their unique ownership. All header/payload readers have drained.
    let cell = unsafe { Box::from_non_null(cell) };
    // SAFETY: the constructor paired this concrete payload pointer with T.
    let payload = unsafe { Box::from_non_null(cell.payload.cast::<T>()) };
    // cell remains the allocation guard if payload destruction panics.
    drop(payload);
    drop(cell);
}

impl<A: 'static, I: Copy + 'static> Drop for ObjectAllocationOwner<A, I> {
    fn drop(&mut self) {
        // Resolve the concrete destructor while the payload is still live.
        // Copy only pointer/function values out of these borrows: recovering
        // the allocation must never free the receiver of a live vtable call.
        let payload = self.cell().payload;
        // SAFETY: this unique owner retains the payload throughout this call.
        let destroy = unsafe { payload.as_ref() }.allocation_destructor();
        // SAFETY: the private blanket implementation selects the same layout
        // as construction. All source borrows above ended before consumption,
        // and the arena has drained every published binding and pin.
        unsafe { destroy(self.cell) };
    }
}

// SAFETY: the payload is Send + Sync, the issuer is an opaque address,
// and moving the owner carries only a Copy Send identity.
unsafe impl<A: 'static, I: Copy + Send + 'static> Send for ObjectAllocationOwner<A, I> {}
// SAFETY: shared access exposes a Sync identity and a Send + Sync payload;
// the issuer is never dereferenced by the owner.
unsafe impl<A: 'static, I: Copy + Sync + 'static> Sync for ObjectAllocationOwner<A, I> {}

impl<A: 'static, I: Copy + 'static> ObjectCell<A, I> {
    #[inline]
    pub fn arena(&self) -> NonNull<A> {
        self.arena
    }

    #[inline]
    pub fn id(&self) -> I {
        self.id
    }

    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub fn type_name(&self) -> &'static str {
        // SAFETY: this header's allocation owner retains the payload.
        unsafe { self.payload.as_ref() }.concrete_type_name()
    }

    #[inline]
    pub fn typed_projection<T: 'static>(&self) -> Option<TypedObjectProjection<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| TypedObjectProjection {
            // Recover the data pointer from the retained owner after checking
            // the cached concrete type. The owner stays present until every
            // binding/read capability and pin has drained.
            pointer: self.payload.cast(),
            _type: PhantomData,
        })
    }
}

// SAFETY: I is Send; the payload is Send + Sync and the issuer is opaque.
unsafe impl<A: 'static, I: Copy + Send + 'static> Send for ObjectCell<A, I> {}
// SAFETY: I is Sync; the payload is Send + Sync and the issuer is opaque.
unsafe impl<A: 'static, I: Copy + Sync + 'static> Sync for ObjectCell<A, I> {}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn miri_owner_moves_without_changing_header_or_checked_projection() {
        struct Payload(Arc<AtomicUsize>);
        impl Drop for Payload {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        struct Issuer;
        let mut issuer = Issuer;
        let issuer_address = NonNull::from(&mut issuer);
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = ObjectAllocationOwner::new(issuer_address, 42_u64, Payload(Arc::clone(&drops)));
        let cell = owner.as_non_null();
        let projection = owner.cell().typed_projection::<Payload>().unwrap();
        let moved = Box::new(owner);
        assert_eq!(moved.as_non_null(), cell);
        assert_eq!(moved.cell().arena(), issuer_address);
        assert_eq!(moved.cell().id(), 42);
        assert!(moved.cell().typed_projection::<u64>().is_none());
        // SAFETY: the exact allocated header/payload remains owned by moved.
        let payload = unsafe { projection.as_ref_unchecked() };
        assert!(Arc::ptr_eq(&payload.0, &drops));
        drop(moved);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
