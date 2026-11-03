//! Linear initialized-memory ownership. Box allocation/recovery and published
//! temporal readers still need the production caller composition described in
//! the worklist; an integer address is never used as an access permission.
use vstd::prelude::*;
use vstd::raw_ptr::ptr_ref;
use super::heap_permission::HeapPermission;

verus! {
pub struct LinearOwner<T> {
    pointer: *mut T,
    permission: Tracked<HeapPermission<T>>,
}

impl<T> LinearOwner<T> {
    pub closed spec fn inv(&self) -> bool {
        self.permission@.inv() && self.permission@.ptr() == self.pointer && self.permission@.is_init()
    }

    pub closed spec fn pointer(&self) -> *mut T { self.pointer }
    pub closed spec fn value(&self) -> T { self.permission@.value() }

    /// Allocation must transfer the actual initialized memory permission.
    /// This function cannot manufacture one from a numeric allocation ID.
    pub fn adopt(pointer: *mut T, Tracked(permission): Tracked<HeapPermission<T>>) -> (owner: Self)
        requires permission.inv(), permission.ptr() == pointer, permission.is_init(),
        ensures owner.inv(), owner.pointer() == pointer, owner.value() == permission.value(),
    {
        LinearOwner { pointer, permission: Tracked(permission) }
    }

    pub fn as_ptr(&self) -> (pointer: *mut T)
        ensures pointer == self.pointer(),
    { self.pointer }

    /// The returned reference borrows the memory permission for the same lifetime.
    pub fn borrow(&self) -> (value: &T)
        requires self.inv(),
        ensures *value == self.value(),
    {
        ptr_ref(self.pointer as *const T, Tracked(self.permission.borrow().borrow()))
    }

    /// Dereference the caller's published pointer with this allocation's rights.
    /// Numeric address equality alone cannot establish this precondition.
    pub fn borrow_at(&self, pointer: *const T) -> (value: &T)
        requires self.inv(), pointer == self.pointer() as *const T,
        ensures *value == self.value(),
    {
        ptr_ref(pointer, Tracked(self.permission.borrow().borrow()))
    }

    /// Transfers the owned memory permission once. This is the resource needed
    /// by a Box recovery adapter, not a proof of Box::from_raw itself.
    pub fn recover(self) -> (permission: Tracked<HeapPermission<T>>)
        requires self.inv(),
        ensures permission@.inv(), permission@.ptr() == self.pointer(), permission@.is_init(),
            permission@.value() == self.value(),
    { self.permission }
}
}
