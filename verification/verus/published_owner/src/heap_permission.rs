//! Owned global-allocator memory, including the right to deallocate it.
use vstd::prelude::*;
use vstd::raw_ptr::{PointsTo, Dealloc, deallocate};
use vstd::layout::{size_of, align_of};
verus! {
broadcast use vstd::raw_ptr::group_raw_ptr_axioms;
pub open spec fn allocation_matches<T>(memory: PointsTo<T>, allocation: Option<Dealloc>) -> bool {
    match allocation {
        Some(a) => size_of::<T>() > 0
            && a.addr() == memory.ptr().addr()
            && a.size() == size_of::<T>() && a.align() == align_of::<T>()
            && a.provenance() == memory.ptr()@.provenance,
        None => size_of::<T>() == 0,
    }
}

pub tracked struct HeapPermission<T> {
    memory: PointsTo<T>,
    allocation: Option<Dealloc>,
}
impl<T> HeapPermission<T> {
    #[verifier::type_invariant]
    pub closed spec fn inv(self) -> bool {
        allocation_matches(self.memory, self.allocation) && self.memory.ptr().addr() != 0
    }
    pub closed spec fn ptr(&self) -> *mut T { self.memory.ptr() }
    pub closed spec fn value(&self) -> T { self.memory.value() }
    pub closed spec fn is_init(&self) -> bool { self.memory.is_init() }
    pub closed spec fn is_uninit(&self) -> bool { self.memory.is_uninit() }

    pub proof fn from_parts(tracked memory: PointsTo<T>, tracked allocation: Option<Dealloc>)
        -> (tracked heap: Self)
        requires allocation_matches(memory, allocation),
        ensures heap.inv(), heap.ptr() == memory.ptr(), heap.is_init() == memory.is_init(),
            heap.is_init() ==> heap.value() == memory.value(),
    {
        memory.is_nonnull();
        HeapPermission { memory, allocation }
    }

    pub proof fn borrow(tracked &self) -> (tracked memory: &PointsTo<T>)
        ensures memory.ptr() == self.ptr(), memory.is_init() == self.is_init(),
            self.is_init() ==> memory.value() == self.value(),
    { &self.memory }

    /// Validate against the real library deallocation primitive contract.
    /// Initialized values must first be destroyed/moved by a separate adapter.
    pub fn free_uninitialized(pointer: *mut T, Tracked(heap): Tracked<Self>)
        requires heap.ptr() == pointer, heap.is_uninit(),
    {
        proof { use_type_invariant(&heap); }
        if core::mem::size_of::<T>() != 0 {
            let tracked HeapPermission { memory, allocation } = heap;
            let tracked raw = memory.into_raw();
            deallocate(pointer as *mut u8, core::mem::size_of::<T>(), core::mem::align_of::<T>(),
                Tracked(raw), Tracked(allocation.tracked_unwrap()));
        }
    }

    /// Ownership recovery must transfer both resources, never only PointsTo.
    pub proof fn into_parts(tracked self) -> (tracked parts: (PointsTo<T>, Option<Dealloc>))
        requires self.inv(),
        ensures allocation_matches(parts.0, parts.1), parts.0.ptr() == self.ptr(),
            parts.0.is_init() == self.is_init(),
            self.is_init() ==> parts.0.value() == self.value(),
    { (self.memory, self.allocation) }
}
}
