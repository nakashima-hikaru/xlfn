use super::*;

struct PayloadDrop(std::sync::Arc<AtomicUsize>);

impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

#[test]
#[allow(
    clippy::drop_non_drop,
    clippy::forget_non_drop,
    reason = "fault injection checks abandoning an affine close proof, including no Drop guarantee"
)]
fn abandoning_closed_cache_owner_retains_payload_until_its_own_drain() {
    for forget in [false, true] {
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let domain = Box::new(CacheLookupDomain::new());
        let node = Box::into_non_null(Box::new(CacheNode {
            value: PayloadDrop(std::sync::Arc::clone(&drops)),
            pins: AtomicU32::new(1),
            resident: AtomicBool::new(false),
            published: true,
            weight: 1,
            generation: 0,
            domain: NonNull::from(&*domain),
        }));
        // SAFETY: this fixture owns the allocation's sole creation pin and
        // retains the boxed domain through final recovery. Enqueue separately
        // so ordinary CacheLease Drop cannot drive maintenance before seal.
        let retired = unsafe { ReclaimEntry::release_pin(node) }.unwrap();
        retired.enqueue();
        assert_eq!(domain.pending_nodes.load(Ordering::Acquire), 1);
        let closed = domain.seal();
        if forget {
            std::mem::forget(closed);
        } else {
            drop(closed);
        }
        assert_eq!(drops.load(Ordering::Acquire), 0);
        let retired = domain.seal().drain();
        assert_eq!(retired.len(), 1);
        reclaim_cache_entries(retired);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(domain.seal().drain().is_empty());
    }
}

#[test]
fn saturated_pending_weight_keeps_backpressure_until_both_queues_empty() {
    let domain = CacheLookupDomain::<()>::new();
    let reader = domain.domain.enter(0).unwrap();
    ReclaimEntry::sentinel(&domain, u64::MAX).enqueue();
    assert!(domain.poll_and_drain().is_none());
    assert_eq!(domain.domain.current_generation().index(), 1);
    ReclaimEntry::sentinel(&domain, u64::MAX).enqueue();
    drop(reader);

    let first = domain.poll_and_drain().unwrap().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 1);
    assert_eq!(domain.pending_weight(), u64::MAX);

    let second = domain.quiesce_and_drain();
    assert_eq!(second.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 0);
    assert_eq!(domain.pending_weight(), 0);
}
