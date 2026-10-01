use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[test]
fn entry_layout_and_thread_bounds() {
    assert_eq!(size_of::<ResidentEntry<u8>>(), size_of::<Entry<u8>>());
    #[cfg(target_pointer_width = "64")]
    assert_eq!(size_of::<CacheNode<u64>>(), 40);
    static_assertions::assert_impl_all!(ResidentEntry<u32>: Send, Sync);
    static_assertions::assert_not_impl_any!(ResidentEntry<Cell<u32>>: Send, Sync);
    static_assertions::assert_not_impl_any!(ResidentEntry<std::rc::Rc<u32>>: Send, Sync);
    static_assertions::assert_not_impl_any!(
        ResidentEntry<std::sync::MutexGuard<'static, ()>>: Send, Sync
    );
}

fn exercise_tag<V: PartialEq + std::fmt::Debug>(value: V) {
    let pointer = Box::into_non_null(Box::new(CacheNode {
        value,
        // Keep a fixture pin after retirement, so the unused domain is never
        // accessed and the fixture can explicitly destroy the allocation.
        pins: AtomicU32::new(2),
        resident: AtomicBool::new(true),
        published: true,
        weight: 7,
        generation: 0,
        domain: NonNull::dangling(),
    }));
    let mut owner = ResidentEntry::new((NodePtr(pointer), 7));
    assert_eq!(owner.tagged_node.addr().get() & 1, 1);
    assert_eq!(owner.snapshot().0.0, pointer);
    assert_eq!(owner.snapshot().1, 7);
    let snapshot = owner.clone();
    assert_eq!(snapshot.tagged_node, pointer);
    assert_eq!(snapshot.snapshot().0.0, pointer);
    assert_eq!(snapshot.snapshot().1, 7);
    drop(snapshot.clone());
    // SAFETY: the fixture pin retains this allocation throughout these reads.
    let node = unsafe { pointer.as_ref() };
    assert_eq!(node.pins.load(Ordering::Relaxed), 2);
    assert!(node.resident.load(Ordering::Relaxed));
    // SAFETY: the snapshot must restore the same live, aligned allocation.
    let snapshot_node = unsafe { snapshot.snapshot().0.0.as_ref() };
    assert_eq!(&snapshot_node.value, &node.value);
    owner.retire();
    owner.retire();
    drop(owner);
    // SAFETY: the fixture pin still owns the allocation. Retirement must have
    // released exactly one pin even after an explicit retry and owner drop.
    let node = unsafe { pointer.as_ref() };
    assert_eq!(node.pins.load(Ordering::Relaxed), 1);
    assert!(!node.resident.load(Ordering::Relaxed));
    // SAFETY: only the fixture pin remains; no reference is used after this.
    unsafe { drop(Box::from_non_null(pointer)) };
    // Non-owning entries may outlive the allocation. Clone, snapshot and drop
    // must only manipulate pointer metadata, never touch the retired node.
    let stale = snapshot.clone();
    assert_eq!(stale.snapshot().0.0, pointer);
    drop(stale);
    drop(snapshot);
}

#[test]
fn miri_pointer_tag_preserves_provenance_and_retires_once() {
    #[repr(align(64))]
    #[derive(Debug, PartialEq)]
    struct Aligned(u8);

    exercise_tag(());
    exercise_tag(42_u8);
    exercise_tag(Aligned(42));
}
