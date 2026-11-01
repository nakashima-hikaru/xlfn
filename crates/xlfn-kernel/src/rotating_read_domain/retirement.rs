//! Append-only registration and lock-free disposal of generation-bound owners.
use super::*;
use std::mem::ManuallyDrop;

mod storage {
    pub trait Sealed {}
    impl<T> Sealed for Vec<T> {}
    impl<A: smallvec::Array> Sealed for smallvec::SmallVec<A> {}
}

/// Audited queue storage. Only the built-in vector implementations can satisfy
/// the private supertrait: arbitrary interior-mutable or custom-drop queues
/// cannot expose registered owners through observation or disposal callbacks.
#[allow(private_bounds, reason = "retirement storage is deliberately sealed")]
pub trait RetirementStorage: storage::Sealed + Default {
    type Item;
    #[doc(hidden)]
    fn len(&self) -> usize;
    #[doc(hidden)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[doc(hidden)]
    fn append_one(&mut self, owner: Self::Item);
    #[doc(hidden)]
    fn append_owned(&mut self, batch: Self);
}

impl<T> RetirementStorage for Vec<T> {
    type Item = T;
    fn len(&self) -> usize {
        self.len()
    }
    fn append_one(&mut self, owner: T) {
        let owner = ManuallyDrop::new(owner);
        self.reserve(1);
        // Successful reservation leaves no fallible allocation before transfer.
        super::queue::append_retired!(self, ManuallyDrop::into_inner(owner));
    }
    fn append_owned(&mut self, batch: Self) {
        let batch = ManuallyDrop::new(batch);
        self.reserve(batch.len());
        // Audited owning iteration invokes no user code, and every push fits.
        for owner in ManuallyDrop::into_inner(batch) {
            super::queue::append_retired!(self, owner);
        }
    }
}

impl<A: smallvec::Array> RetirementStorage for smallvec::SmallVec<A> {
    type Item = A::Item;
    fn len(&self) -> usize {
        self.len()
    }
    fn append_one(&mut self, owner: A::Item) {
        let owner = ManuallyDrop::new(owner);
        self.reserve(1);
        super::queue::append_retired!(self, ManuallyDrop::into_inner(owner));
    }
    fn append_owned(&mut self, batch: Self) {
        let batch = ManuallyDrop::new(batch);
        self.reserve(batch.len());
        for owner in ManuallyDrop::into_inner(batch) {
            super::queue::append_retired!(self, owner);
        }
    }
}

/// Registration authority exposes no queue reference or removal operation.
pub struct RetirementRegistration<'queue, Q: RetirementStorage> {
    queue: MutexGuard<'queue, Q>,
}

impl<'queue, Q: RetirementStorage> RetirementRegistration<'queue, Q> {
    pub(super) fn new(queue: MutexGuard<'queue, Q>) -> Self {
        Self { queue }
    }

    pub fn append(&mut self, owner: Q::Item) {
        self.queue.append_one(owner);
    }
    pub fn append_batch(&mut self, batch: Q) {
        self.queue.append_owned(batch);
    }
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
