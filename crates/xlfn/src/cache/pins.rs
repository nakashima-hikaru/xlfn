//! Pin ownership is distinct from a copied lookup observation.
use super::*;

pub(super) struct ResidentPin<V> {
    node: NodePtr<V>,
}
impl<V> ResidentPin<V> {
    pub(super) fn acquire(creator: &CreatorPinGuard<'_, V>) -> Self {
        Self {
            node: creator.acquire_anchor(),
        }
    }
    /// Transfer into the index's tagged ownership representation. Losing this
    /// pointer leaks a pin; it does not grant release authority to observers.
    pub(super) fn into_index(self) -> NodePtr<V> {
        let owner = std::mem::ManuallyDrop::new(self);
        owner.node
    }
    /// Caller transfers the index's unique tagged residency obligation.
    pub(super) unsafe fn from_index(node: NodePtr<V>) -> Self {
        Self { node }
    }
    pub(super) fn retire(self) {
        drop(self);
    }
}
impl<V> Drop for ResidentPin<V> {
    fn drop(&mut self) {
        retire_resident_raw(self.node);
    }
}

pub(super) struct FlightAnchor<V> {
    node: NodePtr<V>,
}
impl<V> FlightAnchor<V> {
    pub(super) fn acquire(creator: &CreatorPinGuard<'_, V>) -> Self {
        Self {
            node: creator.acquire_anchor(),
        }
    }
    pub(super) fn observe(&self) -> NodePtr<V> {
        self.node
    }
}
impl<V> Drop for FlightAnchor<V> {
    fn drop(&mut self) {
        // SAFETY: this non-Clone owner retains one acquired anchor pin.
        unsafe {
            release_node_pin(self.node.0);
        }
    }
}

// The resident index only selects entries to remove. The residency state,
// pin release, and retirement enqueue remain owned by CalculationCache.
fn retire_resident_raw<V>(node_ptr: NodePtr<V>) {
    // SAFETY: the index transfers its one outstanding residency obligation.
    let node = unsafe { node_ptr.0.as_ref() };
    node.resident.store(false, Ordering::Release);
    // SAFETY: the index transfers its one residency pin exactly once.
    if let Some(entry) = unsafe { ReclaimEntry::release_pin(node_ptr.0) } {
        entry.enqueue();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_owners_preserve_pointer_layout_and_are_not_copyable() {
        static_assertions::assert_not_impl_any!(ResidentPin<u32>: Copy, Clone);
        static_assertions::assert_not_impl_any!(FlightAnchor<u32>: Copy, Clone);
        static_assertions::assert_impl_all!(ResidentPin<u32>: Send, Sync);
        static_assertions::assert_impl_all!(FlightAnchor<u32>: Send, Sync);
        assert_eq!(
            std::mem::size_of::<ResidentPin<u32>>(),
            std::mem::size_of::<NodePtr<u32>>()
        );
        assert_eq!(
            std::mem::size_of::<FlightAnchor<u32>>(),
            std::mem::size_of::<NodePtr<u32>>()
        );
    }
}
