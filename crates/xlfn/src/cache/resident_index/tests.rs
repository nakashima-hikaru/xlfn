use super::*;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

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
    static_assertions::assert_impl_all!(ResidentIndex<String, u32>: Send, Sync);
    static_assertions::assert_impl_all!(ResidentKey<Cell<u32>>: Send);
    static_assertions::assert_not_impl_any!(ResidentKey<Cell<u32>>: Sync);
    static_assertions::assert_not_impl_any!(ResidentKey<std::rc::Rc<u32>>: Send, Sync);
    static_assertions::assert_not_impl_any!(ResidentKey<String>: Clone);
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

struct KeyDrops {
    ids: crate::sync::Mutex<Vec<u32>>,
    fault: AtomicU8,
    panic_on_drop: bool,
}

impl KeyDrops {
    fn fail_once(&self, fault: u8) {
        assert!(
            self.fault
                .compare_exchange(fault, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_err(),
            "injected key callback panic"
        );
    }
}

struct TrackedKey {
    id: u32,
    resident_copy: bool,
    drops: Arc<KeyDrops>,
}

impl TrackedKey {
    fn new(id: u32, drops: &Arc<KeyDrops>) -> Self {
        Self {
            id,
            resident_copy: false,
            drops: Arc::clone(drops),
        }
    }
}

impl Clone for TrackedKey {
    fn clone(&self) -> Self {
        self.drops.fail_once(1);
        Self {
            id: self.id,
            resident_copy: true,
            drops: Arc::clone(&self.drops),
        }
    }
}

impl Hash for TrackedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.drops.fail_once(2);
        // Collisions force the backend's equality path as well as its hashing.
        0_u8.hash(state);
    }
}

impl PartialEq for TrackedKey {
    fn eq(&self, other: &Self) -> bool {
        self.drops.fail_once(3);
        self.id == other.id
    }
}

impl Eq for TrackedKey {}

impl Drop for TrackedKey {
    fn drop(&mut self) {
        if self.resident_copy {
            self.drops.ids.lock().push(self.id);
            assert!(!self.drops.panic_on_drop, "injected retired key drop panic");
        }
    }
}

// A fixture pin keeps each node alive independently of backend residency. The
// domain is intentionally unused: retiring an entry leaves exactly this pin.
struct PinnedNode(NonNull<CacheNode<()>>);

impl PinnedNode {
    fn new(weight: u64) -> Self {
        Self(Box::into_non_null(Box::new(CacheNode {
            value: (),
            pins: AtomicU32::new(2),
            resident: AtomicBool::new(true),
            published: true,
            weight,
            generation: 0,
            domain: NonNull::dangling(),
        })))
    }

    fn entry(&self, weight: u64) -> ResidentEntry<()> {
        ResidentEntry::new((NodePtr(self.0), weight))
    }
}

impl Drop for PinnedNode {
    fn drop(&mut self) {
        // SAFETY: the fixture pin retains the node, and all containing indexes
        // are dropped before the fixture. No pointer is used after recovery.
        let node = unsafe { Box::from_non_null(self.0) };
        assert_eq!(node.pins.load(Ordering::Relaxed), 1);
        assert!(!node.resident.load(Ordering::Relaxed));
    }
}

#[test]
fn miri_fixed_key_owner_survives_moves_eviction_and_backend_drop() {
    for panic_on_drop in [false, true] {
        let drops = Arc::new(KeyDrops {
            ids: crate::sync::Mutex::new(Vec::new()),
            fault: AtomicU8::new(0),
            panic_on_drop,
        });
        // Declare pins before the index so unwind also destroys the backend first.
        let mut nodes = Vec::new();
        let mut indexes = vec![ResidentIndex::<TrackedKey, ()>::new(4)];
        let insert =
            |index: &ResidentIndex<TrackedKey, ()>, nodes: &mut Vec<PinnedNode>, id, weight| {
                nodes.push(PinnedNode::new(weight));
                let key = VersionedKey {
                    epoch: 0,
                    key: TrackedKey::new(id, &drops),
                };
                index.insert_resident(&key, nodes.last().unwrap().entry(weight));
            };
        for id in 0..4 {
            insert(&indexes[0], &mut nodes, id, 1);
        }
        let address = NonNull::from(indexes[0].retired_keys.as_ref().unwrap().as_ref());
        // Grow/move the enclosing owner after its non-owning pointers are stored.
        for _ in 0..8 {
            indexes.push(ResidentIndex::new(1));
        }
        let index = indexes.remove(0);
        assert_eq!(
            NonNull::from(index.retired_keys.as_ref().unwrap().as_ref()),
            address
        );
        // Replacement and explicit removal may detach keys synchronously, but
        // their user destructors wait for maintenance outside the backend call.
        insert(&index, &mut nodes, 1, 1);
        index.invalidate(&VersionedKey {
            epoch: 0,
            key: TrackedKey::new(0, &drops),
        });
        assert!(drops.ids.lock().is_empty());
        index.maintenance();
        assert!(!drops.ids.lock().is_empty());
        // A full-budget entry evicts more than DefaultLifecycle's two slots;
        // rejected oversized input must also retire exactly one key and pin.
        insert(&index, &mut nodes, 4, 4);
        insert(&index, &mut nodes, 5, 5);
        for id in 6..10 {
            insert(&index, &mut nodes, id, 1);
        }
        // No clear: backend destruction itself must finish all pointer users
        // before the owner destroys the queue, even after crossing a thread.
        std::thread::scope(|scope| scope.spawn(move || drop(index)).join().unwrap());
        let mut actual = drops.ids.lock().clone();
        actual.sort_unstable();
        assert_eq!(actual, [0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}

#[test]
fn miri_fixed_key_owner_preserves_retirement_after_key_callback_panics() {
    for fault in [1, 2, 3] {
        let drops = Arc::new(KeyDrops {
            ids: crate::sync::Mutex::new(Vec::new()),
            fault: AtomicU8::new(0),
            panic_on_drop: false,
        });
        let nodes = [PinnedNode::new(1), PinnedNode::new(1)];
        let index = ResidentIndex::<TrackedKey, ()>::new(4);
        let first = VersionedKey {
            epoch: 0,
            key: TrackedKey::new(1, &drops),
        };
        index.insert_resident(&first, nodes[0].entry(1));
        drops.fault.store(fault, Ordering::Relaxed);
        let second = VersionedKey {
            epoch: 0,
            key: TrackedKey::new(2, &drops),
        };
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            index.insert_resident(&second, nodes[1].entry(1));
        }));
        assert!(result.is_err());
        assert!(
            index
                .get(&VersionedKeyRef {
                    epoch: 0,
                    key: &first.key
                })
                .is_some()
        );
        index.maintenance();
        drop(index);
        let expected = if fault == 1 { vec![1] } else { vec![1, 2] };
        let mut actual = drops.ids.lock().clone();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }
}
